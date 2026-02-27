//! Fixed-size audio buffer for real-time use. Allocated once, reused forever; no allocation after construction.

/// Fixed-capacity buffer of f32 samples. Safe to use on the audio thread (no allocation, no locks).
pub struct AudioBuffer {
    storage: Box<[f32]>,
}

impl AudioBuffer {
    /// Creates a new buffer with the given frame count. Allocates once; contents are zeroed.
    /// No allocation ever happens after this.
    pub fn new(frame_count: usize) -> Self {
        let storage: Box<[f32]> = vec![0.0f32; frame_count].into_boxed_slice();
        AudioBuffer { storage }
    }

    /// Returns the number of samples (frames) in the buffer.
    pub fn len(&self) -> usize {
        self.storage.len()
    }

    /// Mutable slice of the buffer for writing samples. Used on the audio thread.
    pub fn as_mut_slice(&mut self) -> &mut [f32] {
        &mut self.storage
    }

    /// Immutable slice of the buffer for reading samples.
    pub fn as_slice(&self) -> &[f32] {
        &self.storage
    }
}

#[cfg(test)]
mod tests {
    use super::AudioBuffer;

    #[test]
    /// Test that the buffer is created with the correct length.
    fn test_new_creates_buffer_with_correct_length() {
        let len = 128;
        let buffer = AudioBuffer::new(len);
        assert_eq!(buffer.len(), len);
    }

    #[test]
    /// Test that the length of the mutable slice matches the capacity of the buffer.
    fn test_as_mut_slice_length_matches_capacity() {
        let len = 128;
        let mut buffer = AudioBuffer::new(len);
        assert_eq!(buffer.as_mut_slice().len(), len);
    }

    #[test]
    /// Test that every element in the buffer is 0.0 after creation.
    fn test_zeroed_after_creation() {
        let len = 128;
        let buffer = AudioBuffer::new(len);
        assert!(buffer.as_slice().iter().all(|&x| x == 0.0));
    }

    #[test]
    /// Test that writing to the mutable slice is visible in the immutable slice.
    fn test_as_mut_slice_writes_visible_in_as_slice() {
        let len = 128;
        let mut buffer = AudioBuffer::new(len);
        buffer.as_mut_slice()[0] = 1.0;
        assert_eq!(buffer.as_slice()[0], 1.0);
    }
}
