# Capstan

A real-time audio processing library. Build graphs of nodes (sine, gain, mixer, delay, biquad, input), compile them for the audio thread, and drive the engine via lock-free commands.

## Quick start

```bash
cargo build
cargo test
cargo run --example cli_daw
```

The **CLI DAW** example is the deliverable: a small CLI-based digital audio workstation. Type commands (patch graph, gain, transport) and press Enter; see `help` in the example for commands.

## Library

Capstan is a library crate. Use it to build your own real-time audio apps: construct an `AudioGraph`, compile it to a `CompiledGraph`, send it to the audio thread with `Command::SwapGraph`, and run the `Engine` in your own audio callback (e.g. using `capstan::cpal` and `stream_config_with_low_latency`). See crate docs and the `cli_daw` example for a full run loop.
