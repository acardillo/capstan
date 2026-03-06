//! Device enumeration and opening an input stream by index. Keeps CPAL usage inside the crate.

use std::sync::Arc;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

use crate::input_buffer::InputSampleBuffer;
use crate::stream_config_with_low_latency;

/// Info for one input device: index (use with [`open_input_stream`]) and display name.
#[derive(Clone, Debug)]
pub struct InputDeviceInfo {
    pub index: usize,
    pub name: String,
}

/// Errors from device listing or opening.
#[derive(Debug)]
pub enum DeviceError {
    NoDeviceAtIndex(usize),
    List(cpal::DevicesError),
    Name(cpal::DeviceNameError),
    Config(cpal::DefaultStreamConfigError),
    Build(cpal::BuildStreamError),
    Play(cpal::PlayStreamError),
}

impl std::fmt::Display for DeviceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DeviceError::NoDeviceAtIndex(i) => write!(f, "no input device at index {}", i),
            DeviceError::List(e) => write!(f, "list devices: {}", e),
            DeviceError::Name(e) => write!(f, "device name: {}", e),
            DeviceError::Config(e) => write!(f, "default config: {}", e),
            DeviceError::Build(e) => write!(f, "build stream: {}", e),
            DeviceError::Play(e) => write!(f, "play stream: {}", e),
        }
    }
}

impl std::error::Error for DeviceError {}

/// Returns a list of input devices: index and name. Use `index` with [`open_input_stream`].
/// On some backends (e.g. ALSA) devices are enumerated one at a time and dropped to avoid
/// holding multiple devices open.
pub fn input_device_list(host: &cpal::Host) -> Result<Vec<InputDeviceInfo>, DeviceError> {
    let mut list = Vec::new();
    for (index, device) in host.input_devices().map_err(DeviceError::List)?.enumerate() {
        let name = device
            .description()
            .map_err(DeviceError::Name)?
            .name()
            .to_string();
        list.push(InputDeviceInfo { index, name });
    }
    Ok(list)
}

/// Opens an input stream for the device at `device_index` (from [`input_device_list`]),
/// feeding samples into `buffer`. Uses low-latency config and F32. The returned stream is
/// already playing; the caller must keep it alive (e.g. store in a variable) for as long
/// as input should be captured.
pub fn open_input_stream(
    host: &cpal::Host,
    device_index: usize,
    buffer: Arc<InputSampleBuffer>,
) -> Result<cpal::Stream, DeviceError> {
    let device = host
        .input_devices()
        .map_err(DeviceError::List)?
        .nth(device_index)
        .ok_or(DeviceError::NoDeviceAtIndex(device_index))?;
    let supported = device.default_input_config().map_err(DeviceError::Config)?;
    if supported.sample_format() != cpal::SampleFormat::F32 {
        return Err(DeviceError::Build(
            cpal::BuildStreamError::StreamConfigNotSupported,
        ));
    }
    let config = stream_config_with_low_latency(&supported);
    let channels = config.channels;
    let err_fn = move |err: cpal::StreamError| eprintln!("input stream error: {}", err);
    let stream = device
        .build_input_stream(
            &config,
            move |data: &[f32], _: &cpal::InputCallbackInfo| {
                buffer.write_block(data, channels);
            },
            err_fn,
            None,
        )
        .map_err(DeviceError::Build)?;
    stream.play().map_err(DeviceError::Play)?;
    Ok(stream)
}
