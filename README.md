# Capstan

[![CI](https://github.com/acardillo/capstan/actions/workflows/ci.yml/badge.svg)](https://github.com/acardillo/capstan/actions/workflows/ci.yml)

A real-time audio processing library. Build graphs of nodes (sine, gain, mixer, delay, biquad, input), compile them for the audio thread, and drive the engine via lock-free commands.

Licensed under the [MIT License](LICENSE).

## Quick start

```bash
cargo build
cargo test
cargo run --example daw
cargo run --example memo
```

## Examples

**Digital Audio Workstation (DAW)**
A CLI-based mini DAW with multiple tracks. Tracks can take input from a device, sine oscillator, or WAV file. Gain can be set per-track and monitored with live ASCII level meters. Run with `cargo run --example daw`.

**Memo**
A simple recorder from the default input. Records to a timestamped WAV file. Run with `cargo run --example memo`.

## Documentation

- **[docs/ARCHITECTURE.md](docs/ARCHITECTURE.md)** — Types, data flow, and how the system is structured.
- **[docs/DESIGN.md](docs/DESIGN.md)** — Why: two threads, lock-free, compiled graph, pull-based file playback.

## Pre-commit (format + lint)

To run `cargo fmt` and `cargo clippy` before every commit, use the git hook (one-time setup):

```bash
git config core.hooksPath .githooks
chmod +x .githooks/pre-commit
```
