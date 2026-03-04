//! Helper to feed an [`InputSampleBuffer`] from an audio file (WAV) in a background thread.
//! Use for file-playback tracks: the buffer can be wired to an [`InputNode`](crate::nodes::InputNode).

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use hound::WavReader;

use crate::input_buffer::InputSampleBuffer;

/// Handle to the file-feeder thread. Dropping it sets a stop flag; the thread will exit
/// after finishing the current block. Call [`FileFeederHandle::join`] to wait for it.
pub struct FileFeederHandle {
    stop: Arc<AtomicBool>,
    join_handle: Option<thread::JoinHandle<()>>,
}

impl FileFeederHandle {
    /// Signals the feeder thread to stop and waits for it to finish.
    pub fn join(mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(h) = self.join_handle.take() {
            let _ = h.join();
        }
    }
}

impl Drop for FileFeederHandle {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

/// Errors from starting the file feeder.
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

const CHUNK: usize = 512;
const PRIME_CHUNKS: usize = 32;

// Playback design: resample with linear interpolation (no overshoot → no clipping/buzz). Sleep
// exactly chunk_duration so feeder matches consumption; the prime (PRIME_CHUNKS) cushions jitter.
// When downsampling, one-pole lowpass reduces aliasing. Index with (j+1)%len for wraparound.

/// One-pole lowpass: y = alpha * x + (1 - alpha) * y_prev. Use when downsampling to avoid aliasing.
fn one_pole_lowpass_coeff(cutoff_hz: f32, sample_rate: u32) -> f32 {
    let rc = 1.0 / (2.0 * std::f32::consts::PI * cutoff_hz);
    let dt = 1.0 / sample_rate as f32;
    1.0 - (-dt / rc).exp()
}

fn resample_chunk(
    mono: &[f32],
    file_rate: u32,
    target_sample_rate: u32,
    out_pos: &mut f64,
    out_buf: &mut [f32],
    lowpass_state: &mut f32,
) {
    let len = mono.len();
    if len == 0 {
        return;
    }

    if file_rate == target_sample_rate {
        let start = *out_pos as usize % len;
        for i in 0..CHUNK {
            out_buf[i] = mono[(start + i) % len];
        }
        *out_pos += CHUNK as f64;
        return;
    }

    let ratio = file_rate as f64 / target_sample_rate as f64;
    let next = |j: usize| (j + 1) % len;
    let linear = |a: f32, b: f32, t: f32| a + t * (b - a);
    let do_lowpass = file_rate > target_sample_rate;
    let alpha = do_lowpass.then(|| {
        one_pole_lowpass_coeff(0.45f32 * target_sample_rate as f32, target_sample_rate)
    });

    for i in 0..CHUNK {
        let src_pos = *out_pos * ratio;
        let j_f = src_pos.floor() as usize;
        let j = j_f % len;
        let frac = (src_pos - j_f as f64) as f32;
        let a = mono[j];
        let b = mono[next(j)];
        let mut v = linear(a, b, frac);
        if let Some(alpha) = alpha {
            *lowpass_state = alpha * v + (1.0 - alpha) * *lowpass_state;
            v = *lowpass_state;
        }
        out_buf[i] = v;
        *out_pos += 1.0;
    }
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

/// Starts a background thread that plays the WAV file into `buffer`. Loads the file and primes the
/// buffer (first PRIME_CHUNKS of output) in the calling thread so playback starts immediately;
/// the spawned thread then continues from there at realtime rate.
pub fn start_file_feeder(
    path: impl AsRef<Path>,
    buffer: Arc<InputSampleBuffer>,
    target_sample_rate: u32,
) -> Result<FileFeederHandle, FileFeederError> {
    let (mono, file_rate) = load_wav_mono(path.as_ref())?;
    let stop = Arc::new(AtomicBool::new(false));
    let stop_clone = Arc::clone(&stop);

    let mut out_pos = 0.0f64;
    let mut out_buf = vec![0.0f32; CHUNK];
    let mut lowpass_state = 0.0f32;

    for _ in 0..PRIME_CHUNKS {
        resample_chunk(
            &mono,
            file_rate,
            target_sample_rate,
            &mut out_pos,
            &mut out_buf,
            &mut lowpass_state,
        );
        buffer.write_block(&out_buf, 1);
    }
    let initial_out_pos = out_pos;
    let initial_lowpass_state = lowpass_state;

    let join_handle = thread::Builder::new()
        .name("capstan_file_feeder".into())
        .spawn(move || {
            run_feeder(
                mono,
                file_rate,
                buffer,
                target_sample_rate,
                stop_clone,
                initial_out_pos,
                initial_lowpass_state,
            )
        })
        .map_err(|e| FileFeederError::Format(e.to_string()))?;

    Ok(FileFeederHandle {
        stop,
        join_handle: Some(join_handle),
    })
}

fn run_feeder(
    mono: Vec<f32>,
    file_rate: u32,
    buffer: Arc<InputSampleBuffer>,
    target_sample_rate: u32,
    stop: Arc<AtomicBool>,
    mut out_pos: f64,
    mut lowpass_state: f32,
) {
    let mut out_buf = vec![0.0f32; CHUNK];
    let chunk_duration = Duration::from_secs_f64(CHUNK as f64 / target_sample_rate as f64);

    while !stop.load(Ordering::Relaxed) {
        resample_chunk(
            &mono,
            file_rate,
            target_sample_rate,
            &mut out_pos,
            &mut out_buf,
            &mut lowpass_state,
        );
        buffer.write_block(&out_buf, 1);
        thread::sleep(chunk_duration);
    }
}
