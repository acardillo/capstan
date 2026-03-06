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

- **`daw`** — CLI-based mini DAW: multiple tracks with device input, sine tone, or WAV file; per-track and master gain; live ASCII level meters. Commands: `track create`, `input <n> --device|--sine|--file`, `gain`, `quit`.
- **`memo`** — Simple recorder from the default input. Press Enter to start recording, Enter again to stop; saves a timestamped WAV (default: `~/Desktop`, or `--out <dir>`).

## Documentation

- **[docs/ARCHITECTURE.md](docs/ARCHITECTURE.md)** — Types, data flow, and how the system is structured.
- **[docs/DESIGN.md](docs/DESIGN.md)** — Why: two threads, lock-free, compiled graph, pull-based file playback.

## Pre-commit (format + lint)

To run `cargo fmt` and `cargo clippy` before every commit, use the git hook (one-time setup):

```bash
git config core.hooksPath .githooks
chmod +x .githooks/pre-commit
```
