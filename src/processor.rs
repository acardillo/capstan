//! Audio node interface. Every source, filter, and processor in the graph implements this trait.

/// Interface for all audio nodes. Implementations must be real-time safe: no allocation, no locks.
pub trait Processor {
    /// Fill `output` with this block of samples. Called on the audio thread each callback.
    fn process(&mut self, output: &mut [f32]);
}

pub struct Silence;

impl Processor for Silence {
    fn process(&mut self, output: &mut [f32]) {
        for sample in output.iter_mut() {
            *sample = 0.0;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Processor, Silence};
    use crate::audio_buffer::AudioBuffer;

    #[test]
    /// Test that the processor can mutate a buffer.
    fn test_processor_can_be_called_with_slice() {
        let buffer_len = 128;

        let mut buffer = AudioBuffer::new(buffer_len);
        for sample in buffer.as_mut_slice().iter_mut() {
            *sample = 1.0;
        }

        let mut silence_processor = Silence;

        silence_processor.process(buffer.as_mut_slice());
        assert!(buffer.as_slice().iter().all(|&x| x == 0.0));
    }

    #[test]
    /// Test that the buffer is the same length after processing.
    fn test_processor_respects_output_length() {
        let buffer_len = 128;
        let mut buffer = AudioBuffer::new(buffer_len);
        let mut silence_processor = Silence;
        silence_processor.process(buffer.as_mut_slice());
        assert_eq!(buffer.len(), buffer_len);
    }
}
