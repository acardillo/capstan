//! Capstan Library root. Audio types (AudioBuffer, Processor, graph, etc.) will live
//! in modules under this crate; the binary in `main.rs` will drive the engine.

pub mod buffer;
pub mod nodes;
pub mod processor;

use cpal::{Sample, SampleFormat};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

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