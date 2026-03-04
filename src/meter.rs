//! Lock-free meter buffer for reporting per-tap peak levels from the audio thread to the control thread.

use std::sync::atomic::{AtomicU32, Ordering};

/// Lock-free buffer of peak levels (one f32 per tap). Audio thread writes with [`MeterBuffer::write_peak`];
/// control thread reads with [`MeterBuffer::read_peaks`]. Used with [`CompiledGraph`](crate::graph::CompiledGraph)
/// meter taps to drive level meters in a UI.
#[derive(Clone)]
pub struct MeterBuffer {
    inner: std::sync::Arc<[AtomicU32]>,
}

unsafe impl Send for MeterBuffer {}
unsafe impl Sync for MeterBuffer {}

impl MeterBuffer {
    /// Creates a meter buffer with `num_taps` slots. Each slot holds one peak value (f32).
    pub fn new(num_taps: usize) -> Self {
        let inner: Vec<AtomicU32> = (0..num_taps).map(|_| AtomicU32::new(0)).collect();
        MeterBuffer {
            inner: inner.into(),
        }
    }

    /// Number of tap slots.
    pub fn len(&self) -> usize {
        self.inner.len()
    }

    /// Writes the peak value for tap `index`. Called from the audio thread after processing a block.
    #[inline]
    pub fn write_peak(&self, index: usize, value: f32) {
        if index < self.inner.len() {
            self.inner[index].store(value.to_bits(), Ordering::Relaxed);
        }
    }

    /// Reads all current peak values. Called from the control thread (e.g. on UI redraw).
    pub fn read_peaks(&self) -> Vec<f32> {
        self.inner
            .iter()
            .map(|a| f32::from_bits(a.load(Ordering::Relaxed)))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::MeterBuffer;

    #[test]
    fn test_meter_buffer_write_read() {
        let buf = MeterBuffer::new(3);
        buf.write_peak(0, 0.25);
        buf.write_peak(1, 0.5);
        buf.write_peak(2, 1.0);
        let peaks = buf.read_peaks();
        assert_eq!(peaks.len(), 3);
        assert!((peaks[0] - 0.25).abs() < 1e-6);
        assert!((peaks[1] - 0.5).abs() < 1e-6);
        assert!((peaks[2] - 1.0).abs() < 1e-6);
    }

    #[test]
    fn test_meter_buffer_out_of_bounds_write() {
        let buf = MeterBuffer::new(2);
        buf.write_peak(5, 1.0);
        let peaks = buf.read_peaks();
        assert_eq!(peaks.len(), 2);
        assert_eq!(peaks[0], 0.0);
        assert_eq!(peaks[1], 0.0);
    }
}
