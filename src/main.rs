//! Capstan CLI — real-time graph updates via stdin.

use std::io::{self, BufRead};
use std::thread;

use capstan::command::{command_channel, Command};
use capstan::event::{event_channel, Event};
use capstan::graph::{AudioGraph, CompiledGraph, GraphNode, NodeId};
use capstan::nodes::{GainProcessor, Mixer, SineGenerator};
use capstan::run_tone_with_command_drain;
use clap::Parser;

const DEFAULT_SAMPLE_RATE: u32 = 48_000;
const DEFAULT_FRAME_COUNT: usize = 512;

#[derive(Parser, Debug)]
#[command(name = "capstan")]
#[command(about = "Real-time audio engine. Type commands and press Enter.")]
struct Cli {
    /// Buffer size for command/event channels
    #[arg(long, default_value = "1024")]
    channel_capacity: usize,
}

fn build_default_graph(freq_hz: f32, gain: f32) -> Option<CompiledGraph> {
    let mut g = AudioGraph::new();
    g.add_node(GraphNode::Sine(SineGenerator::new(freq_hz, DEFAULT_SAMPLE_RATE)));
    g.add_node(GraphNode::Gain(GainProcessor::new(gain)));
    g.add_edge(NodeId::new(0), NodeId::new(1));
    g.compile(DEFAULT_FRAME_COUNT).ok()
}

/// Two sines → mixer. Node order: sine0, sine1, mixer. Gains default to 0.5 each.
fn build_mixer_graph(freq1_hz: f32, freq2_hz: f32, gain1: f32, gain2: f32) -> Option<CompiledGraph> {
    let mut g = AudioGraph::new();
    let s0 = g.add_node(GraphNode::Sine(SineGenerator::new(freq1_hz, DEFAULT_SAMPLE_RATE)));
    let s1 = g.add_node(GraphNode::Sine(SineGenerator::new(freq2_hz, DEFAULT_SAMPLE_RATE)));
    let mix = g.add_node(GraphNode::Mixer(Mixer::new(vec![gain1, gain2])));
    g.add_edge(s0, mix);
    g.add_edge(s1, mix);
    g.compile(DEFAULT_FRAME_COUNT).ok()
}

fn main() {
    let cli = Cli::parse();

    let (cmd_tx, cmd_rx) = command_channel(cli.channel_capacity);
    let (evt_tx, evt_rx) = event_channel(cli.channel_capacity);
    let (shutdown_tx, shutdown_rx) = std::sync::mpsc::channel();

    let audio_handle = thread::spawn(move || {
        run_tone_with_command_drain(cmd_rx, evt_tx, shutdown_rx);
    });

    println!("Capstan — real-time audio. Commands: gain <0-1> | graph [freq] [gain] | graph mix [f1] [f2] [g1] [g2] | quit | resume | help");
    let stdin = io::stdin();
    let mut lines = stdin.lock().lines();

    while let Some(Ok(line)) = lines.next() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }

        let parts: Vec<&str> = line.split_ascii_whitespace().collect();
        match parts.as_slice() {
            ["quit" | "q"] => {
                let _ = cmd_tx.try_send(Command::Quit);
                let _ = shutdown_tx.send(());
                break;
            }
            ["resume" | "r"] => {
                let _ = cmd_tx.try_send(Command::Resume);
                println!("Resumed.");
            }
            ["gain", v] | ["g", v] => {
                if let Ok(g) = v.parse::<f32>() {
                    let _ = cmd_tx.try_send(Command::SetGain(g));
                    println!("Gain set to {}.", g);
                } else {
                    println!("Usage: gain <number>");
                }
            }
            ["graph"] => {
                if let Some(compiled) = build_default_graph(440.0, 0.5) {
                    let _ = cmd_tx.try_send(Command::SwapGraph(compiled));
                    println!("Swapped default graph (440 Hz sine → gain 0.5).");
                } else {
                    eprintln!("Failed to compile graph.");
                }
            }
            ["graph", "mix"] => {
                if let Some(compiled) = build_mixer_graph(440.0, 660.0, 0.5, 0.5) {
                    let _ = cmd_tx.try_send(Command::SwapGraph(compiled));
                    println!("Swapped mixer graph (440 Hz + 660 Hz → mixer 0.5, 0.5).");
                } else {
                    eprintln!("Failed to compile graph.");
                }
            }
            ["graph", "mix", f1, f2] => {
                if let (Ok(a), Ok(b)) = (f1.parse::<f32>(), f2.parse::<f32>()) {
                    if let Some(compiled) = build_mixer_graph(a, b, 0.5, 0.5) {
                        let _ = cmd_tx.try_send(Command::SwapGraph(compiled));
                        println!("Swapped mixer graph ({} Hz + {} Hz → mixer 0.5, 0.5).", a, b);
                    } else {
                        eprintln!("Failed to compile graph.");
                    }
                } else {
                    println!("Usage: graph mix [freq1] [freq2] [gain1] [gain2]");
                }
            }
            ["graph", "mix", f1, f2, g1, g2] => {
                if let (Ok(a), Ok(b), Ok(ga), Ok(gb)) = (
                    f1.parse::<f32>(),
                    f2.parse::<f32>(),
                    g1.parse::<f32>(),
                    g2.parse::<f32>(),
                ) {
                    if let Some(compiled) = build_mixer_graph(a, b, ga, gb) {
                        let _ = cmd_tx.try_send(Command::SwapGraph(compiled));
                        println!("Swapped mixer graph ({} Hz + {} Hz → mixer {}, {}).", a, b, ga, gb);
                    } else {
                        eprintln!("Failed to compile graph.");
                    }
                } else {
                    println!("Usage: graph mix [freq1] [freq2] [gain1] [gain2]");
                }
            }
            ["graph", freq] => {
                if let Ok(f) = freq.parse::<f32>() {
                    if let Some(compiled) = build_default_graph(f, 0.5) {
                        let _ = cmd_tx.try_send(Command::SwapGraph(compiled));
                        println!("Swapped graph ({} Hz sine → gain 0.5).", f);
                    } else {
                        eprintln!("Failed to compile graph.");
                    }
                } else {
                    println!("Usage: graph [freq] [gain]  or  graph mix [f1] [f2] [g1] [g2]");
                }
            }
            ["graph", freq, gain] => {
                if let (Ok(f), Ok(g)) = (freq.parse::<f32>(), gain.parse::<f32>()) {
                    if let Some(compiled) = build_default_graph(f, g) {
                        let _ = cmd_tx.try_send(Command::SwapGraph(compiled));
                        println!("Swapped graph ({} Hz sine → gain {}).", f, g);
                    } else {
                        eprintln!("Failed to compile graph.");
                    }
                } else {
                    println!("Usage: graph [freq] [gain]");
                }
            }
            ["help" | "h" | "?"] => {
                println!("  gain <n>  (g <n>)   Set gain 0–1 (hardcoded chain or last graph)");
                println!("  graph [freq] [gain] Swap in default sine→gain graph (default 440, 0.5)");
                println!("  graph mix [f1] [f2] [g1] [g2]  Two sines → mixer (default 440, 660, 0.5, 0.5)");
                println!("  quit (q)             Stop engine and exit");
                println!("  resume (r)           Resume after quit");
                println!("  help (h)             This message");
            }
            _ => {
                println!("Unknown command. Type 'help' for commands.");
            }
        }

        while let Some(evt) = evt_rx.try_recv() {
            match evt {
                Event::GraphSwapped(_) => {}
                Event::StreamStopped => {}
                Event::NoOp => {}
            }
        }
    }

    let _ = audio_handle.join();
}
