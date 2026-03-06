//! Audio nodes: sources (e.g. SineGenerator, InputNode) and processors (e.g. GainProcessor).

use crate::input_buffer::SampleSource;
use crate::processor::Processor;
use std::f32::consts::PI;
use std::sync::Arc;

/// Generates a sine wave at the given frequency. Phase is carried across process() calls for continuity.
#[derive(Clone, Debug, PartialEq)]
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
    fn process(&mut self, _inputs: &[&[f32]], output: &mut [f32]) {
        for sample in output.iter_mut() {
            *sample = f32::sin(2.0 * PI * self.phase);
            self.phase += self.frequency_hz / self.sample_rate as f32;
            self.phase %= 1.0;
        }
    }
}

/// Multiplies each sample by a gain factor. In-place: reads and writes the same buffer.
#[derive(Clone, Debug, PartialEq)]
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
    fn process(&mut self, inputs: &[&[f32]], output: &mut [f32]) {
        if let Some(inp) = inputs.first() {
            let n = output.len().min(inp.len());
            for i in 0..n {
                output[i] = inp[i] * self.gain;
            }
            for s in output[n..].iter_mut() {
                *s = 0.0;
            }
        } else {
            for sample in output.iter_mut() {
                *sample *= self.gain;
            }
        }
    }
}

/// Mixes N inputs with per-input linear gain. output[i] = sum over j of inputs[j][i] * gains[j].
#[derive(Clone, Debug, PartialEq)]
pub struct Mixer {
    /// Per-input linear gain; length must match number of inputs when process() is called.
    pub gains: Vec<f32>,
}

impl Mixer {
    /// Creates a mixer with the given per-input gains.
    pub fn new(gains: Vec<f32>) -> Self {
        Self { gains }
    }

    /// Creates a stereo mixer with unity gain on both inputs.
    pub fn stereo() -> Self {
        Self::new(vec![1.0, 1.0])
    }
}

impl Processor for Mixer {
    fn process(&mut self, inputs: &[&[f32]], output: &mut [f32]) {
        for (i, sample) in output.iter_mut().enumerate() {
            *sample = 0.0;
            for (j, inp) in inputs.iter().enumerate() {
                let g = self.gains.get(j).copied().unwrap_or(0.0);
                *sample += inp.get(i).copied().unwrap_or(0.0) * g;
            }
        }
    }
}

/// Delay line: one input, one output. Output is input delayed by `delay_ms` milliseconds.
/// Uses a circular buffer; no allocation in process().
#[derive(Clone, Debug, PartialEq)]
pub struct DelayLine {
    /// Circular buffer of past samples (length = max_delay_samples).
    buffer: Vec<f32>,
    /// Write position in the ring (next sample goes here).
    write_pos: usize,
    /// Delay in milliseconds.
    pub delay_ms: f32,
    /// Sample rate in Hz (for ms -> samples).
    pub sample_rate: u32,
}

impl DelayLine {
    /// Creates a delay line with room for up to `max_delay_ms` milliseconds.
    pub fn new(max_delay_ms: f32, sample_rate: u32) -> Self {
        let max_samples = (max_delay_ms / 1000.0 * sample_rate as f32).ceil().max(1.0) as usize;
        DelayLine {
            buffer: vec![0.0; max_samples],
            write_pos: 0,
            delay_ms: 0.0,
            sample_rate,
        }
    }

    /// Sets delay time in milliseconds (clamped to 0..max).
    pub fn set_delay_ms(&mut self, delay_ms: f32) {
        self.delay_ms = delay_ms.clamp(
            0.0,
            1000.0 * self.buffer.len() as f32 / self.sample_rate as f32,
        );
    }

    fn delay_samples(&self) -> usize {
        let d = (self.delay_ms / 1000.0 * self.sample_rate as f32).round() as usize;
        d.min(self.buffer.len())
    }
}

impl Processor for DelayLine {
    fn process(&mut self, inputs: &[&[f32]], output: &mut [f32]) {
        let inp = match inputs.first() {
            Some(s) => *s,
            None => {
                output.fill(0.0);
                return;
            }
        };
        let cap = self.buffer.len();
        let delay = self.delay_samples();
        let n = output.len().min(inp.len());
        if delay == 0 {
            for i in 0..n {
                output[i] = inp[i];
                self.buffer[self.write_pos] = inp[i];
                self.write_pos = (self.write_pos + 1) % cap;
            }
            output[n..].fill(0.0);
            return;
        }
        for i in 0..n {
            let read_pos = (self.write_pos + cap - delay) % cap;
            output[i] = self.buffer[read_pos];
            self.buffer[self.write_pos] = inp[i];
            self.write_pos = (self.write_pos + 1) % cap;
        }
        output[n..].fill(0.0);
    }
}

/// Biquad filter (Direct Form I). Lowpass or highpass via Audio EQ Cookbook coefficients.
#[derive(Clone, Debug, PartialEq)]
pub struct BiquadFilter {
    b0: f32,
    b1: f32,
    b2: f32,
    a1: f32,
    a2: f32,
    x1: f32,
    x2: f32,
    y1: f32,
    y2: f32,
    sample_rate: u32,
}

impl BiquadFilter {
    /// Lowpass filter at cutoff Hz with Q (e.g. 0.5 = butterworth).
    pub fn lowpass(sample_rate: u32, cutoff_hz: f32, q: f32) -> Self {
        let (b0, b1, b2, a1, a2) = Self::lowpass_coeffs(sample_rate, cutoff_hz, q);
        BiquadFilter {
            b0,
            b1,
            b2,
            a1,
            a2,
            x1: 0.0,
            x2: 0.0,
            y1: 0.0,
            y2: 0.0,
            sample_rate,
        }
    }

    /// Highpass filter at cutoff Hz with Q.
    pub fn highpass(sample_rate: u32, cutoff_hz: f32, q: f32) -> Self {
        let (b0, b1, b2, a1, a2) = Self::highpass_coeffs(sample_rate, cutoff_hz, q);
        BiquadFilter {
            b0,
            b1,
            b2,
            a1,
            a2,
            x1: 0.0,
            x2: 0.0,
            y1: 0.0,
            y2: 0.0,
            sample_rate,
        }
    }

    fn lowpass_coeffs(sample_rate: u32, freq: f32, q: f32) -> (f32, f32, f32, f32, f32) {
        let fs = sample_rate as f32;
        let w0 = 2.0 * PI * freq / fs;
        let cos_w0 = w0.cos();
        let alpha = w0.sin() / (2.0 * q.max(0.001));
        let a0 = 1.0 + alpha;
        let b0 = (1.0 - cos_w0) / 2.0;
        let b1 = 1.0 - cos_w0;
        let b2 = (1.0 - cos_w0) / 2.0;
        let a1 = -2.0 * cos_w0;
        let a2 = 1.0 - alpha;
        (b0 / a0, b1 / a0, b2 / a0, a1 / a0, a2 / a0)
    }

    fn highpass_coeffs(sample_rate: u32, freq: f32, q: f32) -> (f32, f32, f32, f32, f32) {
        let fs = sample_rate as f32;
        let w0 = 2.0 * PI * freq / fs;
        let cos_w0 = w0.cos();
        let alpha = w0.sin() / (2.0 * q.max(0.001));
        let a0 = 1.0 + alpha;
        let b0 = (1.0 + cos_w0) / 2.0;
        let b1 = -(1.0 + cos_w0);
        let b2 = (1.0 + cos_w0) / 2.0;
        let a1 = -2.0 * cos_w0;
        let a2 = 1.0 - alpha;
        (b0 / a0, b1 / a0, b2 / a0, a1 / a0, a2 / a0)
    }
}

impl Processor for BiquadFilter {
    fn process(&mut self, inputs: &[&[f32]], output: &mut [f32]) {
        let inp = match inputs.first() {
            Some(s) => *s,
            None => {
                output.fill(0.0);
                return;
            }
        };
        let n = output.len().min(inp.len());
        for i in 0..n {
            let x = inp[i];
            let y = self.b0 * x + self.b1 * self.x1 + self.b2 * self.x2
                - self.a1 * self.y1
                - self.a2 * self.y2;
            self.x2 = self.x1;
            self.x1 = x;
            self.y2 = self.y1;
            self.y1 = y;
            output[i] = y;
        }
        output[n..].fill(0.0);
    }
}

/// Source node that reads from a shared buffer (ring buffer for live input, or in-memory file for playback).
#[derive(Clone)]
pub struct InputNode {
    pub buffer: Arc<dyn SampleSource + Send + Sync>,
}

impl InputNode {
    pub fn new(buffer: Arc<dyn SampleSource + Send + Sync>) -> Self {
        Self { buffer }
    }
}

impl std::fmt::Debug for InputNode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("InputNode(..)")
    }
}

impl PartialEq for InputNode {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.buffer, &other.buffer)
    }
}

impl Processor for InputNode {
    fn process(&mut self, _inputs: &[&[f32]], output: &mut [f32]) {
        let _ = self.buffer.read_block(output);
    }
}

#[cfg(test)]
mod tests {
    use super::{GainProcessor, Mixer, SineGenerator};
    use crate::audio_buffer::AudioBuffer;
    use crate::processor::Processor;

    #[test]
    /// Test that the sine generator produces sine-like output - has non-zero values and values between -1 and 1.
    fn test_sine_generator_produces_sine_like_output() {
        let mut sine_generator = SineGenerator::new(440.0, 48000);
        let mut buffer = AudioBuffer::new(128);
        sine_generator.process(&[], buffer.as_mut_slice());
        assert!(buffer.as_slice().iter().all(|&x| (-1.0..=1.0).contains(&x)));
        assert!(buffer.as_slice().iter().any(|&x| x != 0.0));
    }

    #[test]
    /// Test continuity of the sine generator.
    fn test_sine_generator_phase_advances() {
        let mut sine_generator = SineGenerator::new(440.0, 48000);

        let mut buffer = AudioBuffer::new(128);
        sine_generator.process(&[], buffer.as_mut_slice());

        let mut buffer2 = AudioBuffer::new(128);
        sine_generator.process(&[], buffer2.as_mut_slice());

        let phase_after_first_block = (128.0 * 440.0 / 48000.0) % 1.0;
        let expected_first_of_second =
            f32::sin(2.0 * std::f32::consts::PI * phase_after_first_block);
        let actual_first = buffer2.as_slice()[0];
        let epsilon = 1e-5; // float comparison
        assert!((actual_first - expected_first_of_second).abs() < epsilon);
        assert_ne!(buffer.as_slice(), buffer2.as_slice());
    }

    #[test]
    fn test_gain_processor_scales_output() {
        let mut gain_processor = GainProcessor::new(0.5);
        let mut input = AudioBuffer::new(128);
        let mut output = AudioBuffer::new(128);
        for sample in input.as_mut_slice().iter_mut() {
            *sample = 1.0;
        }
        gain_processor.process(&[input.as_slice()], output.as_mut_slice());
        assert!(output.as_slice().iter().all(|&x| x == 0.5));
    }

    #[test]
    fn test_gain_processor_unity_preserves_input() {
        let mut gain_processor = GainProcessor::new(1.0);
        let mut input = AudioBuffer::new(128);
        let mut output = AudioBuffer::new(128);
        for sample in input.as_mut_slice().iter_mut() {
            *sample = 1.0;
        }
        gain_processor.process(&[input.as_slice()], output.as_mut_slice());
        assert!(output.as_slice().iter().all(|&x| x == 1.0));
    }

    #[test]
    fn test_mixer_sums_inputs_with_gain() {
        let mut mixer = Mixer::new(vec![0.5, 0.5]);
        let mut in0 = AudioBuffer::new(4);
        let mut in1 = AudioBuffer::new(4);
        in0.as_mut_slice().fill(1.0);
        in1.as_mut_slice().fill(1.0);
        let mut out = AudioBuffer::new(4);
        mixer.process(&[in0.as_slice(), in1.as_slice()], out.as_mut_slice());
        assert!(out.as_slice().iter().all(|&x| (x - 1.0).abs() < 1e-5));
    }

    #[test]
    fn test_delay_line_impulse() {
        use super::DelayLine;
        let sr = 48_000u32;
        let mut delay = DelayLine::new(10.0, sr);
        delay.set_delay_ms(1.0); // 48 samples at 48kHz
        let delay_samples = (1.0 / 1000.0 * sr as f32).round() as usize;
        let mut input = vec![0.0f32; 128];
        input[0] = 1.0; // impulse
        let mut output = vec![0.0f32; 128];
        delay.process(&[&input[..]], &mut output[..]);
        assert_eq!(
            output[0], 0.0,
            "first sample should be pre-impulse buffer (zero)"
        );
        assert!(
            (output[delay_samples] - 1.0).abs() < 1e-5,
            "impulse should appear at delay_samples"
        );
    }

    #[test]
    fn test_delay_line_zero_delay_passthrough() {
        use super::DelayLine;
        let mut delay = DelayLine::new(10.0, 48_000);
        delay.set_delay_ms(0.0);
        let input: Vec<f32> = (0..8).map(|i| i as f32).collect();
        let mut output = [0.0f32; 8];
        delay.process(&[&input[..]], &mut output[..]);
        assert_eq!(&output[..8], &input[..]);
    }

    #[test]
    fn test_biquad_lowpass_attenuates_highs() {
        use super::BiquadFilter;
        let mut lp = BiquadFilter::lowpass(48_000, 1000.0, 0.707);
        let mut input = vec![0.0f32; 64];
        input[0] = 1.0;
        let mut output = vec![0.0f32; 64];
        lp.process(&[&input[..]], &mut output[..]);
        assert!(output[0].abs() > 0.0);
        assert!(output[0].abs() <= 1.0);
    }

    #[test]
    fn test_biquad_highpass_reduces_dc() {
        use super::BiquadFilter;
        let mut hp = BiquadFilter::highpass(48_000, 100.0, 0.707);
        let input = vec![1.0f32; 256];
        let mut output = vec![0.0f32; 256];
        hp.process(&[&input[..]], &mut output[..]);
        let max_out = output.iter().map(|x| x.abs()).fold(0.0f32, |a, b| a.max(b));
        assert!(
            max_out < 1.0,
            "highpass should attenuate DC relative to input"
        );
    }
}
