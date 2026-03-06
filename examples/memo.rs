//! Simple CLI memo recorder: press Enter to start, press Enter to stop, save as WAV.
//! Uses the full capstan API: run_audio with a graph (Input → Record), RecordBuffer arm/drain, write_wav.
//!
//! Run: `cargo run --example memo` or `cargo run --example memo -- --out /path/to/dir`

use std::io::{self, Write};
use std::path::PathBuf;
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use capstan::command::{command_channel, Command};
use capstan::event::{event_channel, Event};
use capstan::graph::{AudioGraph, GraphNode};
use capstan::input_buffer::{InputSampleBuffer, SampleSource};
use capstan::nodes::{InputNode, RecordNode};
use capstan::record::{write_wav, RecordBuffer};
use capstan::run_audio;
use clap::Parser;

const FRAME_COUNT: usize = 4096;
const INPUT_RING_CAPACITY: usize = 2048;

#[derive(Parser, Debug)]
#[command(name = "memo")]
#[command(about = "Record audio from the default input; save as WAV on Enter to stop.")]
struct Args {
    /// Output directory for memo-<timestamp>.wav (default: ~/Desktop)
    #[arg(long)]
    out: Option<PathBuf>,
}

fn expand_tilde(path: &str) -> PathBuf {
    if path == "~" || path.starts_with("~/") {
        let home = std::env::var("HOME")
            .or_else(|_| std::env::var("USERPROFILE"))
            .unwrap_or_else(|_| ".".to_string());
        let home = PathBuf::from(home);
        if path == "~" {
            home
        } else {
            home.join(path.trim_start_matches('~').trim_start_matches('/'))
        }
    } else {
        PathBuf::from(path)
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();
    let out_dir = args
        .out
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|| "~/Desktop".to_string());
    let out_dir = expand_tilde(&out_dir);
    std::fs::create_dir_all(&out_dir)?;

    let (cmd_tx, cmd_rx) = command_channel(64);
    let (evt_tx, evt_rx) = event_channel(64);
    let (shutdown_tx, shutdown_rx) = std::sync::mpsc::channel();
    let (audio_result_tx, audio_result_rx) = std::sync::mpsc::channel();

    let input_buffer = Arc::new(InputSampleBuffer::new(INPUT_RING_CAPACITY));
    let input_source: Arc<dyn SampleSource + Send + Sync> = Arc::clone(&input_buffer) as _;
    let record_buf = Arc::new(RecordBuffer::new());

    let mut graph = AudioGraph::new();
    let inp = graph.add_node(GraphNode::Input(InputNode::new(input_source)));
    let rec = graph.add_node(GraphNode::Record(RecordNode::new(Arc::clone(&record_buf))));
    graph.add_edge(inp, rec);
    let compiled = graph.compile(FRAME_COUNT).map_err(|e| e.to_string())?;

    let audio_handle = thread::spawn(move || {
        let result = run_audio(cmd_rx, evt_tx, shutdown_rx, Some(input_buffer));
        let _ = audio_result_tx.send(result);
    });

    let mut sample_rate = 48_000u32;
    for _ in 0..50 {
        if let Ok(Err(e)) = audio_result_rx.try_recv() {
            let _ = audio_handle.join();
            return Err(e.into());
        }
        while let Some(evt) = evt_rx.try_recv() {
            if let Event::StreamStarted(rate) = evt {
                sample_rate = rate;
            }
        }
        thread::sleep(Duration::from_millis(10));
    }

    cmd_tx
        .try_send(Command::SwapGraph(compiled))
        .map_err(|_| "command channel full")?;

    print!("Press Enter to start recording. ");
    io::stdout().flush()?;
    {
        let mut line = String::new();
        io::stdin().read_line(&mut line)?;
    }
    record_buf.set_armed(true);

    print!("Press Enter to stop recording. ");
    io::stdout().flush()?;
    {
        let mut line = String::new();
        io::stdin().read_line(&mut line)?;
    }
    record_buf.set_armed(false);
    // Allow the last block(s) to be written by the audio callback before we drain.
    thread::sleep(Duration::from_millis(150));
    let samples = record_buf.drain();

    let _ = cmd_tx.try_send(Command::Quit);
    let _ = shutdown_tx.send(());

    let _ = audio_handle.join();
    match audio_result_rx.recv() {
        Ok(Ok(())) => {}
        Ok(Err(e)) => return Err(e.into()),
        Err(_) => {
            return Err(std::io::Error::other("audio thread exited without sending result").into())
        }
    }

    let filename = format!(
        "Memo_{}.wav",
        chrono::Local::now().format("%Y-%m-%d_%H-%M-%S")
    );
    let path = out_dir.join(&filename);

    write_wav(&path, &samples, sample_rate)?;

    println!("Saved to {}", path.display());
    Ok(())
}
