//! Capstan Library root. Audio types (AudioBuffer, Processor, graph, etc.) will live
//! in modules under this crate; the binary in `main.rs` will drive the engine.
//!
//! ## Latency
//! End-to-end latency is dominated by **buffer size**, not by CPAL. The callback is invoked
//! once per buffer; at 48 kHz, 128 frames ≈ 2.7 ms, 256 ≈ 5.3 ms. We request a low-latency
//! buffer size when building the stream (see `stream_config_with_low_latency`). The host/driver
//! may still impose a larger minimum; on macOS Core Audio often allows 64–128, on Windows/Linux
//! it depends on the backend. For **input → process → output** with minimal latency you need
//! a duplex setup (input + output, same device and config) and small buffers; CPAL supports
//! that via separate input/output streams or platform-specific duplex where available.

pub mod audio_buffer;
pub mod command;
pub mod input_buffer;
pub mod engine;
pub mod event;
pub mod graph;
pub mod nodes;
pub mod processor;
pub mod ring_buffer;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{BufferSize, Sample, SampleFormat, StreamConfig, SupportedBufferSize};

use crate::nodes::{GainProcessor, SineGenerator};
use crate::processor::Processor;
use crate::command::CommandReceiver;
use crate::engine::Engine;
use crate::event::EventSender;

/// Preferred buffer size in frames for low-latency (≈2.7 ms at 48 kHz). Host may use a larger minimum.
const LOW_LATENCY_BUFFER_FRAMES: u32 = 128;

/// Copies mono (one sample per frame) into device buffer as interleaved multi-channel.
/// data.len() must equal mono.len() * channels.
fn interleave_mono_to_stereo(mono: &[f32], data: &mut [f32], channels: u16) {
    let ch = channels as usize;
    let frames = mono.len();
    if data.len() < frames * ch {
        return;
    }
    if ch == 1 {
        data[..frames].copy_from_slice(mono);
        return;
    }
    for (i, &s) in mono.iter().enumerate() {
        for c in 0..ch {
            data[i * ch + c] = s;
        }
    }
}

/// Builds a `StreamConfig` from the device default and sets a low-latency buffer size when the
/// device reports a range. Uses `LOW_LATENCY_BUFFER_FRAMES` if it lies within the supported
/// range, otherwise the device minimum. If the device reports `Unknown`, requests the fixed
/// size anyway (some backends accept it).
pub fn stream_config_with_low_latency(
    supported: &cpal::SupportedStreamConfig,
) -> StreamConfig {
    let mut config = supported.config();
    let requested = LOW_LATENCY_BUFFER_FRAMES;
    match supported.buffer_size() {
        SupportedBufferSize::Range { min, max } => {
            let size = requested.clamp(*min, *max);
            config.buffer_size = BufferSize::Fixed(size);
        }
        SupportedBufferSize::Unknown => {
            config.buffer_size = BufferSize::Fixed(requested);
        }
    }
    config
}

/// Opens the default output device and runs a stream that outputs silence.
pub fn run_silent_output() {
    // 1. Getting the default host and default output device (see `cpal::default_host()`, `HostTrait`, `DeviceTrait`).
    // 2. Getting the default output stream config and converting it to a `StreamConfig` (or use the supported config as required by `build_output_stream`).
    // 3. Building an output stream with a data callback that fills the buffer with `0.0` for every sample. The callback runs on the audio thread — no allocation, no locks.
    // 4. Starting the stream (e.g. `stream.play()`).
    // 5. Blocking the main thread (e.g. `std::thread::park()` or a loop with `std::thread::sleep`) so the process stays alive and the stream keeps running.
    let host: cpal::Host = cpal::default_host();
    let device: cpal::Device = host.default_output_device().expect("no output device available");
    let supported_config = device.default_output_config().expect("no output config available");
    let sample_format = supported_config.sample_format();
    let config = stream_config_with_low_latency(&supported_config);
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
    let config = stream_config_with_low_latency(&supported_config);
    let sample_rate = config.sample_rate;

    if sample_format != SampleFormat::F32 {
        panic!("run_tone only supports F32 output; device has {:?}", sample_format);
    }

    let mut sine = SineGenerator::new(440.0, sample_rate);
    let mut gain = GainProcessor::new(0.5);
    let mut scratch = vec![0.0f32; 8192];

    let err_fn = move |err: cpal::StreamError| eprintln!("output stream error: {}", err);
    let stream = device
        .build_output_stream(
            &config,
            move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
                let n = data.len().min(scratch.len());
                sine.process(&[], &mut scratch[..n]);
                gain.process(&[&scratch[..n]], data);
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
/// If `input_buffer` is `Some`, the default input device is opened and its callback
/// writes blocks into the buffer (for use by graphs that contain an `Input` node).
pub fn run_tone_with_command_drain(
    cmd_rx: CommandReceiver,
    evt_tx: EventSender,
    shutdown: std::sync::mpsc::Receiver<()>,
    input_buffer: Option<std::sync::Arc<crate::input_buffer::InputSampleBuffer>>,
) {
    let host = cpal::default_host();
    let device = host.default_output_device().expect("no output device available");
    let supported_config = device
        .default_output_config()
        .expect("no output config available");
    let sample_format = supported_config.sample_format();
    let config = stream_config_with_low_latency(&supported_config);
    let sample_rate = config.sample_rate;

    if sample_format != SampleFormat::F32 {
        panic!(
            "run_tone_with_command_drain only supports F32 output; device has {:?}",
            sample_format
        );
    }

    let mut engine = Engine::new(sample_rate, 440.0, 0.5);
    let channels = config.channels;
    let mut mono_buf = vec![0.0f32; 4096];

    if let Some(ref buf) = input_buffer {
        if let Some(input_device) = host.default_input_device() {
            if let Ok(supported_input) = input_device.default_input_config() {
                if supported_input.sample_format() == SampleFormat::F32 {
                    let input_config = stream_config_with_low_latency(&supported_input);
                    let in_ch = input_config.channels;
                    let buf_clone = std::sync::Arc::clone(buf);
                    let err_fn = move |err: cpal::StreamError| eprintln!("input stream error: {}", err);
                    match input_device.build_input_stream(
                        &input_config,
                        move |data: &[f32], _: &cpal::InputCallbackInfo| {
                            buf_clone.write_block(data, in_ch);
                        },
                        err_fn,
                        None,
                    ) {
                        Ok(input_stream) => {
                            let _ = input_stream.play();
                            let _input_stream = input_stream;
                            let err_fn_out = move |err: cpal::StreamError| eprintln!("output stream error: {}", err);
                            let out_stream = device
                                .build_output_stream(
                                    &config,
                                    move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
                                        let frames = data.len() / channels as usize;
                                        let mono = mono_buf[..frames].as_mut();
                                        engine.process_audio(&cmd_rx, &evt_tx, mono);
                                        interleave_mono_to_stereo(mono, data, channels);
                                    },
                                    err_fn_out,
                                    None,
                                )
                                .expect("failed to build output stream");
                            out_stream.play().expect("failed to start output stream");
                            let _ = shutdown.recv();
                            return;
                        }
                        Err(e) => eprintln!("failed to build input stream: {}", e),
                    }
                }
            }
        }
    }

    let err_fn = move |err: cpal::StreamError| eprintln!("output stream error: {}", err);
    let stream = device
        .build_output_stream(
            &config,
            move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
                let frames = data.len() / channels as usize;
                let mono = mono_buf[..frames].as_mut();
                engine.process_audio(&cmd_rx, &evt_tx, mono);
                interleave_mono_to_stereo(mono, data, channels);
            },
            err_fn,
            None,
        )
        .expect("failed to build output stream");

    stream.play().expect("failed to start stream");
    let _ = shutdown.recv();
}