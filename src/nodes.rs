//! Audio nodes: sources (e.g. SineGenerator) and processors (e.g. GainProcessor).

use crate::processor::Processor;
use std::f32::consts::PI;

/// Generates a sine wave at the given frequency. Phase is carried across process() calls for continuity.
pub struct SineGenerator {
    /// Frequency in Hz (e.g. 440.0).
    pub frequency_hz: f32,
    /// Sample rate in Hz (e.g. 48_000). Must match the stream.
    pub sample_rate: u32,
    /// Phase in [0.0, 1.0). Advance by frequency_hz / sample_rate per sample; wrap at 1.0.
    pub phase: f32,
}

impl SineGenerator {
    /// Creates a sine generator. Phase starts at 0.0.
    pub fn new(frequency_hz: f32, sample_rate: u32) -> Self {
        Self {
            frequency_hz,
            sample_rate,
            phase: 0.0,
        }
    }
}

impl Processor for SineGenerator {
    fn process(&mut self, output: &mut [f32]) {
        for sample in output.iter_mut() {
            *sample = f32::sin(2.0 * PI * self.phase);
            self.phase += self.frequency_hz / self.sample_rate as f32;
            self.phase %= 1.0;
        }
    }
}

/// Multiplies each sample by a gain factor. In-place: reads and writes the same buffer.
pub struct GainProcessor {
    /// Linear gain (1.0 = unity, 0.0 = silence).
    pub gain: f32,
}

impl GainProcessor {
    /// Creates a gain processor with the given linear gain.
    pub fn new(gain: f32) -> Self {
        Self { gain }
    }
}

impl Processor for GainProcessor {
    fn process(&mut self, output: &mut [f32]) {
        for sample in output.iter_mut() {
            *sample *= self.gain;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{GainProcessor, SineGenerator};
    use crate::processor::Processor;
    use crate::buffer::AudioBuffer;

    #[test]
    /// Test that the sine generator produces sine-like output - has non-zero values and values between -1 and 1.
    fn test_sine_generator_produces_sine_like_output() {
        let mut sine_generator = SineGenerator::new(440.0, 48000);
        let mut buffer = AudioBuffer::new(128);
        sine_generator.process(buffer.as_mut_slice());
        assert!(buffer.as_slice().iter().all(|&x| x >= -1.0 && x <= 1.0));
        assert!(buffer.as_slice().iter().any(|&x| x != 0.0));
    }

    #[test]
    /// Test continuity of the sine generator.
    fn test_sine_generator_phase_advances() {
        let mut sine_generator = SineGenerator::new(440.0, 48000);

        let mut buffer = AudioBuffer::new(128);
        sine_generator.process(buffer.as_mut_slice());

        let mut buffer2 = AudioBuffer::new(128);
        sine_generator.process(buffer2.as_mut_slice());

        let phase_after_first_block = (128.0 * 440.0 / 48000.0) % 1.0;
        let expected_first_of_second = f32::sin(2.0 * std::f32::consts::PI * phase_after_first_block);
        let actual_first = buffer2.as_slice()[0];
        let epsilon = 1e-5;  // float comparison
        assert!((actual_first - expected_first_of_second).abs() < epsilon);
        assert_ne!(buffer.as_slice(), buffer2.as_slice());
    }

    #[test]
    /// Test that the gain processor scales the output.
    fn test_gain_processor_scales_output() {
        let mut gain_processor = GainProcessor::new(0.5);
        let mut buffer = AudioBuffer::new(128);
        for sample in buffer.as_mut_slice().iter_mut() {
            *sample = 1.0;
        }
        gain_processor.process(buffer.as_mut_slice());
        assert!(buffer.as_slice().iter().all(|&x| x == 0.5));
    }

    #[test]
    /// Test that the buffer is unchanged when the gain is 1.0.
    fn test_gain_processor_unity_preserves_input() {
        let mut gain_processor = GainProcessor::new(1.0);
        let mut buffer = AudioBuffer::new(128);
        for sample in buffer.as_mut_slice().iter_mut() {
            *sample = 1.0;
        }
        gain_processor.process(buffer.as_mut_slice());
        assert!(buffer.as_slice().iter().all(|&x| x == 1.0));
    }
}
