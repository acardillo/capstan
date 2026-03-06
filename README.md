# Capstan

[![CI](https://github.com/acardillo/capstan/actions/workflows/ci.yml/badge.svg)](https://github.com/acardillo/capstan/actions/workflows/ci.yml)

A real-time audio processing library. Build graphs of nodes (sine, gain, mixer, delay, biquad, input), compile them for the audio thread, and drive the engine via lock-free commands.

## Quick start

```bash
cargo build
cargo test
cargo run --example daw
cargo run --example memo
```

- **[docs/ARCHITECTURE.md](ARCHITECTURE.md)** — Types, data flow, and how the system is structured.
- **[docs/DESIGN.md](DESIGN.md)** — Why: two threads, lock-free, compiled graph, pull-based file playback.

## Pre-commit (format + lint)

To run `cargo fmt` and `cargo clippy` before every commit, use the git hook (one-time setup):

```bash
git config core.hooksPath .githooks
chmod +x .githooks/pre-commit
```
