//! Load WAV files and resample for use with [`FilePlaybackBuffer`](crate::input_buffer::FilePlaybackBuffer).

use std::path::Path;

use hound::WavReader;

/// Errors from loading or resampling WAV files.
#[derive(Debug)]
pub enum FileFeederError {
    Open(hound::Error),
    Format(String),
}

impl std::fmt::Display for FileFeederError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FileFeederError::Open(e) => write!(f, "open WAV: {}", e),
            FileFeederError::Format(s) => write!(f, "format: {}", s),
        }
    }
}

impl std::error::Error for FileFeederError {}

/// Load WAV from path into mono f32. Returns (mono_samples, file_sample_rate).
fn load_wav_mono(path: &Path) -> Result<(Vec<f32>, u32), FileFeederError> {
    let reader = WavReader::open(path).map_err(FileFeederError::Open)?;
    let spec = reader.spec();
    let file_rate = spec.sample_rate;
    let channels = spec.channels as usize;
    let samples: Vec<f32> = match spec.sample_format {
        hound::SampleFormat::Int => {
            let max = 1 << (spec.bits_per_sample - 1);
            reader
                .into_samples::<i32>()
                .filter_map(Result::ok)
                .map(|s| s as f32 / max as f32)
                .collect()
        }
        hound::SampleFormat::Float => reader
            .into_samples::<f32>()
            .filter_map(Result::ok)
            .map(|s| s.clamp(-1.0, 1.0))
            .collect(),
    };
    let mono: Vec<f32> = if channels == 2 {
        samples.chunks(2).map(|c| (c[0] + c[1]) * 0.5).collect()
    } else {
        samples
    };
    if mono.is_empty() {
        return Err(FileFeederError::Format("file has no samples".to_string()));
    }
    Ok((mono, file_rate))
}

/// One-pole lowpass: y = alpha * x + (1 - alpha) * y_prev. Use when downsampling to avoid aliasing.
fn one_pole_lowpass_coeff(cutoff_hz: f32, sample_rate: u32) -> f32 {
    let rc = 1.0 / (2.0 * std::f32::consts::PI * cutoff_hz);
    let dt = 1.0 / sample_rate as f32;
    1.0 - (-dt / rc).exp()
}

/// Resamples `mono` (at `file_rate`) to `target_sample_rate`, returning a new buffer.
/// Uses linear interpolation; applies one-pole lowpass when downsampling.
pub fn resample_to_rate(
    mono: &[f32],
    file_rate: u32,
    target_sample_rate: u32,
) -> Vec<f32> {
    let len = mono.len();
    if len == 0 {
        return Vec::new();
    }
    if file_rate == target_sample_rate {
        return mono.to_vec();
    }
    let ratio = file_rate as f64 / target_sample_rate as f64;
    let out_len = (len as f64 / ratio).round() as usize;
    let mut out = Vec::with_capacity(out_len);
    let next = |j: usize| (j + 1) % len;
    let linear = |a: f32, b: f32, t: f32| a + t * (b - a);
    let do_lowpass = file_rate > target_sample_rate;
    let alpha = do_lowpass.then(|| {
        one_pole_lowpass_coeff(0.45f32 * target_sample_rate as f32, target_sample_rate)
    });
    let mut lowpass_state = 0.0f32;

    for i in 0..out_len {
        let src_pos = i as f64 * ratio;
        let j_f = src_pos.floor() as usize;
        let j = j_f % len;
        let frac = (src_pos - j_f as f64) as f32;
        let a = mono[j];
        let b = mono[next(j)];
        let mut v = linear(a, b, frac);
        if let Some(alpha) = alpha {
            lowpass_state = alpha * v + (1.0 - alpha) * lowpass_state;
            v = lowpass_state;
        }
        out.push(v);
    }
    out
}

/// Loads a WAV file and returns mono f32 samples at `target_sample_rate` (resampling if needed).
/// Use with [`crate::input_buffer::FilePlaybackBuffer`] for pull-based playback (no feeder thread).
pub fn load_wav_at_rate(
    path: impl AsRef<Path>,
    target_sample_rate: u32,
) -> Result<Vec<f32>, FileFeederError> {
    let (mono, file_rate) = load_wav_mono(path.as_ref())?;
    Ok(resample_to_rate(&mono, file_rate, target_sample_rate))
}
