//! Capstan Library root. Audio types (AudioBuffer, Processor, graph, etc.) will live
//! in modules under this crate; the binary in `main.rs` will drive the engine.

pub mod audio_buffer;
pub mod command;
pub mod engine;
pub mod event;
pub mod graph;
pub mod nodes;
pub mod processor;
pub mod ring_buffer;

use cpal::{Sample, SampleFormat};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

use crate::nodes::{GainProcessor, SineGenerator};
use crate::processor::Processor;
use crate::command::CommandReceiver;
use crate::engine::Engine;
use crate::event::EventSender;

/// Opens the default output device and runs a stream that outputs silence.
pub fn run_silent_output() {
    // 1. Getting the default host and default output device (see `cpal::default_host()`, `HostTrait`, `DeviceTrait`).
    // 2. Getting the default output stream config and converting it to a `StreamConfig` (or use the supported config as required by `build_output_stream`).
    // 3. Building an output stream with a data callback that fills the buffer with `0.0` for every sample. The callback runs on the audio thread — no allocation, no locks.
    // 4. Starting the stream (e.g. `stream.play()`).
    // 5. Blocking the main thread (e.g. `std::thread::park()` or a loop with `std::thread::sleep`) so the process stays alive and the stream keeps running.
    let host: cpal::Host = cpal::default_host();
    let device: cpal::Device = host.default_output_device().expect("no output device available");
    let supported_config: cpal::SupportedStreamConfig = device.default_output_config().expect("no output config available");
    let sample_format: cpal::SampleFormat = supported_config.sample_format();
    let config: cpal::StreamConfig = supported_config.into();
    let err_fn = move |err: cpal::StreamError| eprintln!("an error occurred on the output stream: {}", err);

    let stream: cpal::Stream = match sample_format {
        SampleFormat::F32 => device.build_output_stream(&config, write_silence::<f32>, err_fn, None),
        SampleFormat::I16 => device.build_output_stream(&config, write_silence::<i16>, err_fn, None),
        SampleFormat::U16 => device.build_output_stream(&config, write_silence::<u16>, err_fn, None),
        sample_format => panic!("Unsupported sample format '{sample_format}'")
    }.expect("failed to build output stream");

    stream.play().expect("failed to start stream");
    std::thread::park();
}

/// Writes silence to the data buffer for the given sample format.
fn write_silence<T: Sample>(data: &mut [T], _: &cpal::OutputCallbackInfo) {
    for sample in data.iter_mut() {
        *sample = Sample::EQUILIBRIUM;
    }
}

/// Runs the hardcoded chain SineGenerator → GainProcessor → cpal output. Only supports F32.
/// Creates a stream, runs the chain in the callback (sine then gain in-place on the buffer), then blocks.
pub fn run_tone() {
    let host = cpal::default_host();
    let device = host.default_output_device().expect("no output device available");
    let supported_config = device.default_output_config().expect("no output config available");
    let sample_format = supported_config.sample_format();
    let config: cpal::StreamConfig = supported_config.into();
    let sample_rate = config.sample_rate;

    if sample_format != SampleFormat::F32 {
        panic!("run_tone only supports F32 output; device has {:?}", sample_format);
    }

    let mut sine = SineGenerator::new(440.0, sample_rate);
    let mut gain = GainProcessor::new(0.5);

    let err_fn = move |err: cpal::StreamError| eprintln!("output stream error: {}", err);
    let stream = device
        .build_output_stream(
            &config,
            move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
                sine.process(data);
                gain.process(data);
            },
            err_fn,
            None,
        )
        .expect("failed to build output stream");

    stream.play().expect("failed to start stream");
    std::thread::park();
}

/// Like `run_tone()`, but drains commands and supports graph swap. Blocks until `shutdown`
/// receives a message (then drops the stream and returns). Pass `evt_tx` so the control
/// thread can receive `Event::GraphSwapped(prev)` when a new graph is applied.
pub fn run_tone_with_command_drain(
    cmd_rx: CommandReceiver,
    evt_tx: EventSender,
    shutdown: std::sync::mpsc::Receiver<()>,
) {
    let host = cpal::default_host();
    let device = host.default_output_device().expect("no output device available");
    let supported_config = device
        .default_output_config()
        .expect("no output config available");
    let sample_format = supported_config.sample_format();
    let config: cpal::StreamConfig = supported_config.into();
    let sample_rate = config.sample_rate;

    if sample_format != SampleFormat::F32 {
        panic!(
            "run_tone_with_command_drain only supports F32 output; device has {:?}",
            sample_format
        );
    }

    let mut engine = Engine::new(sample_rate, 440.0, 0.5);

    let err_fn = move |err: cpal::StreamError| eprintln!("output stream error: {}", err);
    let stream = device
        .build_output_stream(
            &config,
            move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
                engine.process_audio(&cmd_rx, &evt_tx, data);
            },
            err_fn,
            None,
        )
        .expect("failed to build output stream");

    stream.play().expect("failed to start stream");
    let _ = shutdown.recv();
}