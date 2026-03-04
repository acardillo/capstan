//! Example: CLI-based DAW (MVP) — tracks, input devices, per-track and master gain.
//! Sticky header at top lists tracks and draws live level meters (ASCII).
//!
//! Run with: `cargo run --example cli_daw`

use std::collections::HashMap;
use std::io::{self, Write};
use std::path::PathBuf;
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use capstan::command::{command_channel, Command};
use capstan::device::{input_device_list, open_input_stream, DeviceError};
use capstan::event::event_channel;
use capstan::file_feeder::load_wav_at_rate;
use capstan::graph::{AudioGraph, CompiledGraph, GraphNode};
use capstan::input_buffer::{FilePlaybackBuffer, InputSampleBuffer, SampleSource};
use capstan::meter::MeterBuffer;
use capstan::nodes::{GainProcessor, InputNode, Mixer, SineGenerator};
use capstan::run_audio;
use clap::Parser;
use crossterm::cursor::MoveTo;
use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use crossterm::execute;
use crossterm::terminal::{disable_raw_mode, enable_raw_mode, Clear, ClearType};

const DEFAULT_FRAME_COUNT: usize = 512;
const METER_WIDTH: usize = 24;
const HEADER_REDRAW_MS: u64 = 80;
/// dB range for the bar: bar is empty at DB_MIN, full at 0 dB. Makes the meter more sensitive to low levels.
const METER_DB_MIN: f32 = -60.0;

#[derive(Parser, Debug)]
#[command(name = "capstan-cli-daw")]
#[command(about = "Capstan CLI DAW (MVP) — tracks, input devices, gain.")]
struct Cli {
    #[arg(long, default_value = "1024")]
    channel_capacity: usize,
}

/// Source for a track: none, device input, sine tone, or file playback.
enum TrackSource {
    None,
    Device(usize),
    Sine { freq_hz: f32 },
    File {
        path: PathBuf,
        buffer: Arc<dyn SampleSource + Send + Sync>,
    },
}

struct Track {
    source: TrackSource,
    gain: f32,
}

/// Open input streams: keep streams alive and map device index -> buffer.
struct OpenInputs {
    _streams: Vec<cpal::Stream>,
    device_to_buffer: HashMap<usize, Arc<dyn SampleSource + Send + Sync>>,
}

impl OpenInputs {
    fn new() -> Self {
        OpenInputs {
            _streams: Vec::new(),
            device_to_buffer: HashMap::new(),
        }
    }

    fn ensure_device(&mut self, host: &cpal::Host, device_index: usize) -> Result<Arc<dyn SampleSource + Send + Sync>, DeviceError> {
        if let Some(buf) = self.device_to_buffer.get(&device_index) {
            return Ok(Arc::clone(buf));
        }
        let buffer: Arc<InputSampleBuffer> = Arc::new(InputSampleBuffer::new(2048));
        let stream = open_input_stream(host, device_index, Arc::clone(&buffer))?;
        self._streams.push(stream);
        let buffer_dyn: Arc<dyn SampleSource + Send + Sync> = buffer;
        self.device_to_buffer.insert(device_index, Arc::clone(&buffer_dyn));
        Ok(buffer_dyn)
    }

    fn get_buffer(&self, device_index: usize) -> Option<Arc<dyn SampleSource + Send + Sync>> {
        self.device_to_buffer.get(&device_index).cloned()
    }
}

fn build_session_graph(
    tracks: &[Track],
    open_inputs: &OpenInputs,
    silent_buffer: &Arc<dyn SampleSource + Send + Sync>,
    master_gain: f32,
    meter_buffer: Option<Arc<MeterBuffer>>,
    sample_rate: u32,
) -> Option<CompiledGraph> {
    let mut g = AudioGraph::new();
    if tracks.is_empty() {
        let inp = g.add_node(GraphNode::Input(InputNode::new(Arc::clone(silent_buffer))));
        let out = g.add_node(GraphNode::Gain(GainProcessor::new(0.0)));
        g.add_edge(inp, out);
        return g.compile(DEFAULT_FRAME_COUNT).ok();
    }

    let mut gain_node_ids = Vec::with_capacity(tracks.len());
    for track in tracks {
        let source_node = match &track.source {
            TrackSource::None => g.add_node(GraphNode::Input(InputNode::new(Arc::clone(silent_buffer)))),
            TrackSource::Device(d) => {
                let buf = open_inputs
                    .get_buffer(*d)
                    .unwrap_or_else(|| Arc::clone(silent_buffer));
                g.add_node(GraphNode::Input(InputNode::new(buf)))
            }
            TrackSource::Sine { freq_hz } => g.add_node(GraphNode::Sine(SineGenerator::new(
                *freq_hz,
                sample_rate,
            ))),
            TrackSource::File { buffer, .. } => g.add_node(GraphNode::Input(InputNode::new(Arc::clone(buffer)))),
        };
        let gain = g.add_node(GraphNode::Gain(GainProcessor::new(track.gain)));
        g.add_edge(source_node, gain);
        gain_node_ids.push(gain);
    }

    let gains: Vec<f32> = tracks.iter().map(|t| t.gain).collect();
    let mix = g.add_node(GraphNode::Mixer(Mixer::new(gains)));
    for &gid in &gain_node_ids {
        g.add_edge(gid, mix);
    }
    let master = g.add_node(GraphNode::Gain(GainProcessor::new(master_gain)));
    g.add_edge(mix, master);

    let compiled = match &meter_buffer {
        Some(mb) if mb.len() == tracks.len() => {
            let n = tracks.len();
            let tap_indices: Vec<usize> = (0..n).map(|i| n + i).collect();
            g.compile_with_meter(DEFAULT_FRAME_COUNT, Some((tap_indices, Arc::clone(mb)))).ok()
        }
        _ => g.compile(DEFAULT_FRAME_COUNT).ok(),
    };
    compiled
}

fn source_display(source: &TrackSource) -> String {
    match source {
        TrackSource::None => "-".to_string(),
        TrackSource::Device(d) => format!("dev {}", d),
        TrackSource::Sine { freq_hz } => format!("{} Hz", freq_hz),
        TrackSource::File { path, .. } => path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("file")
            .to_string(),
    }
}

fn send_graph(cmd_tx: &capstan::command::CommandSender, compiled: CompiledGraph) {
    let _ = cmd_tx.try_send(Command::SwapGraph(compiled));
}

/// Linear peak (0..=1 or higher) to dB. Uses a floor for near-silence.
fn peak_to_db(peak: f32) -> f32 {
    if peak <= 1e-10 {
        METER_DB_MIN
    } else {
        (20.0 * peak.log10()).max(METER_DB_MIN)
    }
}

/// Draw an ASCII level bar scaled in dB (more sensitive to low levels) and append the dB value.
fn ascii_meter_with_db(peak: f32) -> String {
    let db = peak_to_db(peak);
    let normalized = (db - METER_DB_MIN) / (-METER_DB_MIN);
    let fill = (normalized * (METER_WIDTH as f32)).round() as usize;
    let fill = fill.min(METER_WIDTH);
    let bar: String = (0..METER_WIDTH)
        .map(|i| if i < fill { '#' } else { '-' })
        .collect();
    format!("[{}] {:>6.1} dB", bar, db)
}

/// Prompt row is directly under the tracks: table header + track rows.
fn prompt_row(tracks: &[Track]) -> u16 {
    (1 + tracks.len().max(1)).min(200) as u16
}

fn draw_header(
    stdout: &mut impl Write,
    tracks: &[Track],
    peaks: &[f32],
    prompt_row: u16,
) -> std::io::Result<()> {
    let mut line = 0u16;
    if tracks.is_empty() {
        execute!(stdout, MoveTo(0, 0), Clear(ClearType::CurrentLine))?;
        writeln!(stdout, " tracks: (none)")?;
        line += 1;
    } else {
        execute!(stdout, MoveTo(0, 0), Clear(ClearType::CurrentLine))?;
        writeln!(stdout, " track | source     | gain  | level")?;
        line += 1;
        for (i, track) in tracks.iter().enumerate() {
            let src = source_display(&track.source);
            let peak = peaks.get(i).copied().unwrap_or(0.0);
            execute!(stdout, MoveTo(0, line), Clear(ClearType::CurrentLine))?;
            writeln!(
                stdout,
                "   {}   | {:>10} | {:.2}  | {}",
                i + 1,
                src,
                track.gain,
                ascii_meter_with_db(peak)
            )?;
            line += 1;
        }
    }
    execute!(stdout, MoveTo(0, line), Clear(ClearType::CurrentLine))?;
    for y in line..prompt_row {
        execute!(stdout, MoveTo(0, y), Clear(ClearType::CurrentLine))?;
    }
    Ok(())
}

fn draw_history(stdout: &mut impl Write, history: &[String], start_row: u16, max_lines: usize) -> std::io::Result<()> {
    let take = history.len().saturating_sub(max_lines);
    let lines = &history[take..];
    for (i, s) in lines.iter().enumerate() {
        let row = start_row + i as u16;
        execute!(stdout, MoveTo(0, row), Clear(ClearType::CurrentLine))?;
        let truncate = 200;
        let display = if s.len() > truncate {
            format!("{}...", &s[..truncate])
        } else {
            s.clone()
        };
        writeln!(stdout, "{}", display)?;
    }
    for i in lines.len()..max_lines {
        execute!(stdout, MoveTo(0, start_row + i as u16), Clear(ClearType::CurrentLine))?;
    }
    Ok(())
}

const HISTORY_LINES: usize = 20;

fn main() -> std::io::Result<()> {
    let cli = Cli::parse();

    let (cmd_tx, cmd_rx) = command_channel(cli.channel_capacity);
    let (evt_tx, evt_rx) = event_channel(cli.channel_capacity);
    let (shutdown_tx, shutdown_rx) = std::sync::mpsc::channel();
    let host = capstan::cpal::default_host();

    let audio_handle = thread::spawn(move || {
        run_audio(cmd_rx, evt_tx, shutdown_rx, None);
    });

    let mut output_sample_rate = capstan::default_output_sample_rate().unwrap_or(48_000);

    let mut tracks: Vec<Track> = Vec::new();
    let mut master_gain: f32 = 0.8;
    let mut open_inputs = OpenInputs::new();
    let silent_buffer: Arc<dyn SampleSource + Send + Sync> = Arc::new(InputSampleBuffer::new(2048));
    let mut meter_buffer: Option<Arc<MeterBuffer>> = None;
    let mut input_line = String::new();
    let mut status_msg = String::new();
    let mut history: Vec<String> = Vec::new();

    enable_raw_mode().map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;
    let mut stdout = io::stdout();

    execute!(stdout, Clear(ClearType::All), MoveTo(0, 0))
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;
    stdout.flush()?;

    loop {
        let pr = prompt_row(&tracks);
        let peaks = meter_buffer
            .as_ref()
            .map(|m| m.read_peaks())
            .unwrap_or_default();
        draw_header(&mut stdout, &tracks, &peaks, pr)?;
        execute!(stdout, MoveTo(0, pr), Clear(ClearType::CurrentLine))?;
        write!(stdout, "> {}", input_line)?;
        draw_history(&mut stdout, &history, pr + 1, HISTORY_LINES)?;
        stdout.flush()?;

        if event::poll(Duration::from_millis(HEADER_REDRAW_MS))
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?
        {
            if let Ok(Event::Key(ke)) = event::read() {
                if ke.kind != KeyEventKind::Press {
                    continue;
                }
                match ke.code {
                    KeyCode::Enter => {
                        let line = input_line.trim().to_string();
                        input_line.clear();
                        if !line.is_empty() {
                            let parts: Vec<&str> = line.split_ascii_whitespace().collect();
                            let mut session_changed = false;

                            match parts.as_slice() {
                                ["quit" | "q"] => {
                                    let _ = cmd_tx.try_send(Command::Quit);
                                    let _ = shutdown_tx.send(());
                                    disable_raw_mode()
                                        .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;
                                    let _ = audio_handle.join();
                                    return Ok(());
                                }
                                ["help" | "h" | "?"] => {
                                    status_msg = "track create | track delete <no> | input --list | input <tn> --device <n> | input <tn> --sine <hz> | input <tn> --file <path> | gain [tn] <lvl> | quit".to_string();
                                }

                                ["track", "create"] => {
                                    tracks.push(Track {
                                        source: TrackSource::None,
                                        gain: 0.7,
                                    });
                                    session_changed = true;
                                    status_msg = format!("Created track {}.", tracks.len());
                                }
                                ["track", "delete", no] => {
                                    if let Ok(n) = no.parse::<usize>() {
                                        if n >= 1 && n <= tracks.len() {
                                            tracks.remove(n - 1);
                                            session_changed = true;
                                            status_msg = format!("Deleted track {}.", n);
                                        } else {
                                            status_msg = format!("Track number must be 1–{}.", tracks.len().max(1));
                                        }
                                    } else {
                                        status_msg = "Usage: track delete <track_no>".to_string();
                                    }
                                }

                                ["input", "--list" | "-l"] => {
                                    match input_device_list(&host) {
                                        Ok(devices) => {
                                            if devices.is_empty() {
                                                status_msg = "(no input devices)".to_string();
                                            } else {
                                                status_msg = devices
                                                    .iter()
                                                    .map(|d| format!("{}: {}", d.index, d.name))
                                                    .collect::<Vec<_>>()
                                                    .join("  |  ");
                                                if status_msg.len() > 120 {
                                                    status_msg = format!("{}...", &status_msg[..117]);
                                                }
                                            }
                                        }
                                        Err(e) => status_msg = format!("List devices: {}", e),
                                    }
                                }
                                ["input", track_no, "--device", dev] => {
                                    if let (Ok(tn), Ok(d)) =
                                        (track_no.parse::<usize>(), dev.parse::<usize>())
                                    {
                                        if tn >= 1 && tn <= tracks.len() {
                                            match open_inputs.ensure_device(&host, d) {
                                                Ok(_) => {
                                                    tracks[tn - 1].source =
                                                        TrackSource::Device(d);
                                                    session_changed = true;
                                                    status_msg =
                                                        format!("Track {} → device {}.", tn, d);
                                                }
                                                Err(e) => {
                                                    status_msg =
                                                        format!("Failed to open device {}: {}", d, e);
                                                }
                                            }
                                        } else {
                                            status_msg = format!(
                                                "Track number must be 1–{}.",
                                                tracks.len().max(1)
                                            );
                                        }
                                    } else {
                                        status_msg =
                                            "Usage: input <track_no> --device <index>".to_string();
                                    }
                                }
                                ["input", track_no, "--sine", freq] => {
                                    if let (Ok(tn), Ok(f)) =
                                        (track_no.parse::<usize>(), freq.parse::<f32>())
                                    {
                                        if tn >= 1 && tn <= tracks.len() && f > 0.0 && f <= 20_000.0
                                        {
                                            tracks[tn - 1].source =
                                                TrackSource::Sine { freq_hz: f };
                                            session_changed = true;
                                            status_msg =
                                                format!("Track {} → sine {} Hz.", tn, f);
                                        } else if tn < 1 || tn > tracks.len() {
                                            status_msg = format!(
                                                "Track number must be 1–{}.",
                                                tracks.len().max(1)
                                            );
                                        } else {
                                            status_msg = "Frequency must be 0–20000 Hz.".to_string();
                                        }
                                    } else {
                                        status_msg =
                                            "Usage: input <track_no> --sine <freq_hz>".to_string();
                                    }
                                }
                                _ if parts.len() >= 4
                                    && parts[0] == "input"
                                    && parts[2] == "--file" =>
                                {
                                    let path_str = parts[3..].join(" ");
                                    if let Ok(tn) = parts[1].parse::<usize>() {
                                        if tn >= 1 && tn <= tracks.len() {
                                            let path = PathBuf::from(&path_str);
                                            match load_wav_at_rate(&path, output_sample_rate) {
                                                Ok(samples) => {
                                                    let buffer: Arc<dyn SampleSource + Send + Sync> =
                                                        Arc::new(FilePlaybackBuffer::new(Arc::new(samples)));
                                                    tracks[tn - 1].source = TrackSource::File {
                                                        path,
                                                        buffer,
                                                    };
                                                    session_changed = true;
                                                    status_msg =
                                                        format!("Track {} → file {}.", tn, path_str);
                                                }
                                                Err(e) => {
                                                    status_msg = format!("File load: {}", e);
                                                }
                                            }
                                        } else {
                                            status_msg = format!(
                                                "Track number must be 1–{}.",
                                                tracks.len().max(1)
                                            );
                                        }
                                    } else {
                                        status_msg =
                                            "Usage: input <track_no> --file <path>".to_string();
                                    }
                                }

                                ["gain", level] => {
                                    if let Ok(g) = level.parse::<f32>() {
                                        master_gain = g.clamp(0.0, 2.0);
                                        session_changed = true;
                                        status_msg = format!("Master gain set to {}.", master_gain);
                                    } else {
                                        status_msg = "Usage: gain <level>  or  gain <track_no> <level>".to_string();
                                    }
                                }
                                ["gain", track_no, level] => {
                                    if let (Ok(tn), Ok(g)) =
                                        (track_no.parse::<usize>(), level.parse::<f32>())
                                    {
                                        if tn >= 1 && tn <= tracks.len() {
                                            tracks[tn - 1].gain = g.clamp(0.0, 2.0);
                                            session_changed = true;
                                            status_msg =
                                                format!("Track {} gain set to {}.", tn, tracks[tn - 1].gain);
                                        } else {
                                            status_msg =
                                                format!("Track number must be 1–{}.", tracks.len().max(1));
                                        }
                                    } else {
                                        status_msg = "Usage: gain <track_no> <level>".to_string();
                                    }
                                }

                                _ => status_msg = "Unknown command. Type 'help' for commands.".to_string(),
                            }

                            if session_changed {
                                meter_buffer = if tracks.is_empty() {
                                    None
                                } else {
                                    Some(Arc::new(MeterBuffer::new(tracks.len())))
                                };
                                if let Some(compiled) = build_session_graph(
                                    &tracks,
                                    &open_inputs,
                                    &silent_buffer,
                                    master_gain,
                                    meter_buffer.clone(),
                                    output_sample_rate,
                                ) {
                                    send_graph(&cmd_tx, compiled);
                                } else {
                                    status_msg = "Failed to compile graph.".to_string();
                                }
                            }

                            history.push(format!("> {}", line));
                            history.push(format!("  {}", status_msg));
                        }
                    }
                    KeyCode::Char(c) => {
                        input_line.push(c);
                    }
                    KeyCode::Backspace => {
                        input_line.pop();
                    }
                    _ => {}
                }
            }
        }

        while let Some(evt) = evt_rx.try_recv() {
            if let capstan::event::Event::StreamStarted(sr) = evt {
                output_sample_rate = sr;
                history.push(format!("  Output sample rate: {} Hz", sr));
            }
        }
    }
}
