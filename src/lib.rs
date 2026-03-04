//! Capstan — real-time audio processing library. Audio types (AudioBuffer, Processor, graph, etc.)
//! live in modules under this crate.
//!
//! ## Examples
//!
//! See the `cli_daw` example for a full CLI-based DAW that drives the engine via stdin:
//! `cargo run --example cli_daw`.
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
pub mod device;
pub mod file_feeder;
pub mod input_buffer;
pub mod engine;
pub mod event;
pub mod graph;
pub mod meter;
pub mod nodes;
pub mod processor;
pub mod ring_buffer;

/// Re-export for advanced use (custom streams, device enumeration). Most apps should use [`run_audio`].
pub use cpal;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{BufferSize, SampleFormat, StreamConfig, SupportedBufferSize};

use crate::command::CommandReceiver;
use crate::engine::Engine;
use crate::event::EventSender;
use crate::input_buffer::InputSampleBuffer;

/// Preferred buffer size in frames for low-latency (≈2.7 ms at 48 kHz). Host may use a larger minimum.
const LOW_LATENCY_BUFFER_FRAMES: u32 = 128;

/// Copies a mono buffer (one sample per frame) into an interleaved multi-channel buffer.
/// `mono.len()` frames are written; `data.len()` must be at least `mono.len() * channels`.
/// For stereo, each frame is duplicated to L and R.
pub fn interleave_mono_to_stereo(mono: &[f32], data: &mut [f32], channels: u16) {
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

/// Returns the sample rate (Hz) that [`run_audio`] will use for the default output device,
/// or `None` if no device/config is available. Use this when starting file feeders or
/// building graphs so playback matches the actual output rate.
pub fn default_output_sample_rate() -> Option<u32> {
    let host = cpal::default_host();
    let device = host.default_output_device()?;
    let supported = device.default_output_config().ok()?;
    let config = stream_config_with_low_latency(&supported);
    Some(config.sample_rate)
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

/// Runs the audio engine with the default output device (and optionally the default input device).
/// Blocks until `shutdown` receives a message, then returns. All CPAL setup and the audio callback
/// are handled inside the crate; the control thread only needs to pass the command/event channels
/// and a shutdown receiver.
///
/// Requires F32 output. If `input_buffer` is `Some`, the default input device is opened and
/// its callback feeds the buffer (for graphs that use an `Input` node).
pub fn run_audio(
    cmd_rx: CommandReceiver,
    evt_tx: EventSender,
    shutdown: std::sync::mpsc::Receiver<()>,
    input_buffer: Option<std::sync::Arc<InputSampleBuffer>>,
) {
    let host = cpal::default_host();
    let device = host
        .default_output_device()
        .expect("no output device available");
    let supported_config = device
        .default_output_config()
        .expect("no output config available");
    let sample_format = supported_config.sample_format();
    let config = stream_config_with_low_latency(&supported_config);
    let sample_rate = config.sample_rate;
    let _ = evt_tx.try_send(crate::event::Event::StreamStarted(sample_rate));

    if sample_format != SampleFormat::F32 {
        panic!(
            "run_audio only supports F32 output; device has {:?}",
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
                    let err_fn =
                        move |err: cpal::StreamError| eprintln!("input stream error: {}", err);
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
                            let err_fn_out =
                                move |err: cpal::StreamError| eprintln!("output stream error: {}", err);
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

    let err_fn =
        move |err: cpal::StreamError| eprintln!("output stream error: {}", err);
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