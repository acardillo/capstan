//! Recording through the audio graph and writing WAV files.
//!
//! Add a [`RecordNode`](crate::nodes::RecordNode) with a shared [`RecordBuffer`] to your graph.
//! Arm the buffer to start, disarm to stop, then [`drain`](RecordBuffer::drain) and
//! [`write_wav`] to save.

use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

/// Default capacity: ~5.5 minutes at 48 kHz. No allocation on the audio thread.
const DEFAULT_RECORD_CAPACITY: usize = 1 << 24; // 16_777_216 samples

/// Errors from writing WAV.
#[derive(Debug)]
pub enum RecordError {
    Wav(hound::Error),
}

impl std::fmt::Display for RecordError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RecordError::Wav(e) => write!(f, "wav: {}", e),
        }
    }
}

impl std::error::Error for RecordError {}

impl From<hound::Error> for RecordError {
    fn from(e: hound::Error) -> Self {
        RecordError::Wav(e)
    }
}

/// Lock-free SPSC ring for recording. Audio thread writes via [`write_block`](RecordBuffer::write_block)
/// when armed; control thread arms/disarms and drains. No mutex or allocation on the audio path.
pub struct RecordBuffer {
    armed: AtomicBool,
    storage: Box<[std::cell::UnsafeCell<f32>]>,
    cap: usize,
    write_pos: AtomicUsize,
    read_pos: AtomicUsize,
}

unsafe impl Send for RecordBuffer {}
unsafe impl Sync for RecordBuffer {}

impl RecordBuffer {
    /// Creates a new record buffer (disarmed) with default capacity (~5.5 min at 48 kHz).
    pub fn new() -> Self {
        Self::with_capacity(DEFAULT_RECORD_CAPACITY)
    }

    /// Creates a new record buffer with the given capacity (samples). When full, oldest samples are dropped.
    pub fn with_capacity(capacity: usize) -> Self {
        assert!(capacity > 0);
        let storage = (0..capacity)
            .map(|_| std::cell::UnsafeCell::new(0.0f32))
            .collect::<Vec<_>>()
            .into_boxed_slice();
        RecordBuffer {
            armed: AtomicBool::new(false),
            storage,
            cap: capacity,
            write_pos: AtomicUsize::new(0),
            read_pos: AtomicUsize::new(0),
        }
    }

    /// Call from the audio thread each block. When armed, copies samples into the ring (lock-free, no alloc).
    #[inline]
    pub fn write_block(&self, samples: &[f32]) {
        if !self.armed.load(Ordering::Relaxed) {
            return;
        }
        let frames = samples.len();
        if frames == 0 {
            return;
        }
        let write = self.write_pos.load(Ordering::Relaxed);
        let read = self.read_pos.load(Ordering::Acquire);
        let used = write.wrapping_sub(read);
        let free = self.cap.saturating_sub(used);
        let (advance_read, to_write) = if frames > free {
            (frames - free, frames)
        } else {
            (0, frames)
        };
        if advance_read > 0 {
            self.read_pos
                .store(read.wrapping_add(advance_read), Ordering::Release);
        }
        for (i, &s) in samples[..to_write].iter().enumerate() {
            let idx = (write.wrapping_add(i)) % self.cap;
            unsafe { *self.storage[idx].get() = s };
        }
        self.write_pos
            .store(write.wrapping_add(to_write), Ordering::Release);
    }

    /// Arm (true) or disarm (false) recording. Call from the control thread.
    pub fn set_armed(&self, armed: bool) {
        self.armed.store(armed, Ordering::Relaxed);
    }

    /// Returns true when recording is armed.
    pub fn is_armed(&self) -> bool {
        self.armed.load(Ordering::Relaxed)
    }

    /// Reads all samples recorded so far and clears the logical buffer. Call from the control thread
    /// after disarming. Returns the recorded mono samples (empty if none).
    pub fn drain(&self) -> Vec<f32> {
        let read = self.read_pos.load(Ordering::Relaxed);
        let write = self.write_pos.load(Ordering::Acquire);
        let available = write.wrapping_sub(read);
        if available == 0 {
            return Vec::new();
        }
        let mut out = Vec::with_capacity(available);
        for i in 0..available {
            let idx = (read.wrapping_add(i)) % self.cap;
            out.push(unsafe { *self.storage[idx].get() });
        }
        self.read_pos
            .store(read.wrapping_add(available), Ordering::Release);
        out
    }
}

impl Default for RecordBuffer {
    fn default() -> Self {
        Self::new()
    }
}

/// Writes mono f32 samples to a WAV file at the given path.
/// Overwrites the file if it exists.
pub fn write_wav(
    path: impl AsRef<Path>,
    samples: &[f32],
    sample_rate: u32,
) -> Result<(), RecordError> {
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate,
        bits_per_sample: 32,
        sample_format: hound::SampleFormat::Float,
    };
    let mut writer = hound::WavWriter::create(path.as_ref(), spec)?;
    for &s in samples {
        writer.write_sample(s)?;
    }
    writer.finalize()?;
    Ok(())
}
