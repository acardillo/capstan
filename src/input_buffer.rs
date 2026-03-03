//! Lock-free sample ring for passing input audio to the output callback. Input writes
//! (first channel only); output reads exactly the samples it needs. Pre-allocated, real-time safe.

use std::sync::atomic::{AtomicUsize, Ordering};

/// Lock-free SPSC ring of f32 samples (mono, first channel of input). Input callback
/// writes with write_block(); output callback reads with read_block() — reads exactly
/// out.len() samples (or fewer on underrun, rest zeroed). Avoids block-size mismatch artifacts.
pub struct InputSampleBuffer {
    storage: Box<[std::cell::UnsafeCell<f32>]>,
    cap: usize,
    write_pos: AtomicUsize,
    read_pos: AtomicUsize,
}

unsafe impl Send for InputSampleBuffer {}
unsafe impl Sync for InputSampleBuffer {}

impl InputSampleBuffer {
    /// Capacity in samples (mono). Should be large enough for input/output size mismatch (e.g. 4096).
    pub fn new(capacity: usize) -> Self {
        assert!(capacity > 0);
        let storage = (0..capacity)
            .map(|_| std::cell::UnsafeCell::new(0.0f32))
            .collect::<Vec<_>>()
            .into_boxed_slice();
        InputSampleBuffer {
            storage,
            cap: capacity,
            write_pos: AtomicUsize::new(0),
            read_pos: AtomicUsize::new(0),
        }
    }

    /// Called from the input stream callback. Pushes first channel of `data` (interleaved)
    /// into the ring. Drops oldest samples if full.
    pub fn write_block(&self, data: &[f32], channels: u16) {
        let ch = channels as usize;
        if ch == 0 || data.len() < ch {
            return;
        }
        let frames = data.len() / ch;
        let write = self.write_pos.load(Ordering::Relaxed);
        let read = self.read_pos.load(Ordering::Acquire);
        let used = write.wrapping_sub(read);
        let free = self.cap.saturating_sub(used);
        if frames > free {
            self.read_pos
                .store(read.wrapping_add(frames - free), Ordering::Release);
        }
        for i in 0..frames {
            let sample = data[i * ch];
            let idx = (write.wrapping_add(i)) % self.cap;
            unsafe { *self.storage[idx].get() = sample };
        }
        self.write_pos
            .store(write.wrapping_add(frames), Ordering::Release);
    }

    /// Called from the output callback. Reads up to out.len() samples into out.
    /// Returns the number of samples read; rest of out is zeroed. No block alignment — reads exactly what output needs.
    pub fn read_block(&self, out: &mut [f32]) -> usize {
        let read = self.read_pos.load(Ordering::Relaxed);
        let write = self.write_pos.load(Ordering::Acquire);
        let available = write.wrapping_sub(read);
        let n = out.len().min(available).min(self.cap);
        for i in 0..n {
            let idx = (read.wrapping_add(i)) % self.cap;
            out[i] = unsafe { *self.storage[idx].get() };
        }
        out[n..].fill(0.0);
        self.read_pos.store(read.wrapping_add(n), Ordering::Release);
        n
    }
}

#[cfg(test)]
mod tests {
    use super::InputSampleBuffer;

    #[test]
    fn test_write_then_read_returns_data() {
        let buf = InputSampleBuffer::new(64);
        let data: Vec<f32> = (0..32).map(|i| i as f32).collect();
        buf.write_block(&data, 1);
        let mut out = vec![0.0f32; 32];
        assert_eq!(buf.read_block(&mut out), 32);
        assert_eq!(&out[..32], &data[..]);
    }

    #[test]
    fn test_read_empty_fills_silence() {
        let buf = InputSampleBuffer::new(64);
        let mut out = vec![1.0f32; 16];
        assert_eq!(buf.read_block(&mut out), 0);
        assert!(out.iter().all(|&s| s == 0.0));
    }

    #[test]
    fn test_stereo_first_channel_only() {
        let buf = InputSampleBuffer::new(64);
        let data = [1.0f32, -1.0, 2.0, -2.0, 3.0, -3.0]; // 3 frames stereo
        buf.write_block(&data, 2);
        let mut out = vec![0.0f32; 4];
        assert_eq!(buf.read_block(&mut out), 3);
        assert_eq!(out[0], 1.0);
        assert_eq!(out[1], 2.0);
        assert_eq!(out[2], 3.0);
        assert_eq!(out[3], 0.0);
    }
}
