# Capstan

[![CI](https://github.com/acardillo/capstan/actions/workflows/ci.yml/badge.svg)](https://github.com/acardillo/capstan/actions/workflows/ci.yml)

A real-time audio processing library. Build graphs of nodes (sine, gain, mixer, delay, biquad, input), compile them for the audio thread, and drive the engine via lock-free commands.

Licensed under the [MIT License](LICENSE).

## Quick Start Example

Play a 440 Hz tone for 2 seconds, then exit.

```rust,no_run
use capstan::command::{command_channel, Command};
use capstan::event::event_channel;
use capstan::graph::{AudioGraph, GraphNode};
use capstan::nodes::{GainProcessor, SineGenerator};
use capstan::run_audio;
use std::thread;
use std::time::Duration;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let (cmd_tx, cmd_rx) = command_channel(64);
    let (evt_tx, evt_rx) = event_channel(64);
    let (shutdown_tx, shutdown_rx) = std::sync::mpsc::channel();

    let audio_handle = thread::spawn(move || {
        run_audio(cmd_rx, evt_tx, shutdown_rx, None).ok()
    });

    // Wait for stream to start so we know the sample rate (optional; 48_000 is a safe default).
    let sample_rate = evt_rx.recv().ok().and_then(|e| {
        if let capstan::event::Event::StreamStarted(r) = e { Some(r) } else { None }
    }).unwrap_or(48_000);

    let mut graph = AudioGraph::new();
    let sine = graph.add_node(GraphNode::Sine(SineGenerator::new(440.0, sample_rate)));
    let gain = graph.add_node(GraphNode::Gain(GainProcessor::new(0.3)));
    graph.add_edge(sine, gain);
    let compiled = graph.compile(128)?;
    let _ = cmd_tx.try_send(Command::SwapGraph(compiled));

    thread::sleep(Duration::from_secs(2));

    let _ = cmd_tx.try_send(Command::Quit);
    let _ = shutdown_tx.send(());
    let _ = audio_handle.join();
    Ok(())
}
```

## Example applications

### Digital Audio Workstation (DAW) CLI

A CLI-based mini DAW with multiple tracks. Tracks can take input from a device, sine oscillator, or WAV file. Tracks have gain, echo, tremolo, and overdrive settings, and live ASCII level meters.

```bash
cargo run --example daw
```

### Memo Recorder CLI

A simple recorder from the default input. Records to a timestamped WAV file on the Desktop.

```bash
cargo run --example memo
```

## Documentation

- **[docs/ARCHITECTURE.md](docs/ARCHITECTURE.md)** — Types, data flow, and how the system is structured.
- **[docs/DESIGN.md](docs/DESIGN.md)** — Why: two threads, lock-free, compiled graph, pull-based file playback.
- **[docs/PERFORMANCE.md](docs/PERFORMANCE.md)** — Performance and limits: audio-thread rules, callback size, graph size, ring buffer capacities.

## Development

**Build and test**

```bash
cargo build
cargo test
```

**Benchmarks** (graph compile and ring buffer throughput)

```bash
cargo bench
```

**Run examples**

```bash
cargo run --example daw
cargo run --example memo
```

**Pre-commit (format + lint)**

One-time setup to run `cargo fmt` and `cargo clippy` before every commit:

```bash
git config core.hooksPath .githooks
chmod +x .githooks/pre-commit
```
