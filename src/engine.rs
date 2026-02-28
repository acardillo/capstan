//! Minimal engine scaffolding for running a processor chain on the audio thread
//! while draining control `Command`s at the top of each callback.

use crate::command::{Command, CommandReceiver};
use crate::nodes::{GainProcessor, SineGenerator};
use crate::processor::Processor;

/// A minimal, hardcoded “engine” state for Phase 2/early Phase 3.
///
/// The intent is to show the structure the real engine will follow:
/// - drain all pending commands at the start of each callback
/// - apply commands to local state (no locks, no allocation)
/// - render the audio block
pub struct Engine {
    sine_generator: SineGenerator,
    gain_processor: GainProcessor,
    should_quit: bool,
}

impl Engine {
    pub fn new(sample_rate: u32, frequency_hz: f32, initial_gain: f32) -> Self {
        Engine {
            sine_generator: SineGenerator::new(frequency_hz, sample_rate),
            gain_processor: GainProcessor::new(initial_gain),
            should_quit: false,
        }
    }

    /// Drain all currently pending commands and apply them.
    /// This should be called at the *top* of each audio callback, before generating samples.
    pub fn drain_commands(&mut self, cmd_rx: &CommandReceiver) {
        while let Some(cmd) = cmd_rx.try_recv() {
            self.apply_command(cmd);
        }
    }

    /// Render one block of audio into `output`.
    pub fn render_block(&mut self, output: &mut [f32]) {
        self.sine_generator.process(output);
        self.gain_processor.process(output);
    }

    /// Full audio callback: drain commands, then either output silence (if quit) or render.
    /// Call this from the cpal stream callback so quit and render logic stay in one place.
    pub fn process_audio(&mut self, cmd_rx: &CommandReceiver, output: &mut [f32]) {
        self.drain_commands(cmd_rx);
        if self.should_quit() {
            for s in output.iter_mut() {
                *s = 0.0;
            }
        } else {
            self.render_block(output);
        }
    }

    /// Apply a single command to engine state.
    ///
    /// Keeping this logic in a separate function makes it easier to unit test
    /// without involving `cpal`.
    pub fn apply_command(&mut self, cmd: Command) {
        match cmd {
            Command::SetGain(gain) => self.gain_processor.gain = gain,
            Command::Quit => self.should_quit = true,
            Command::Resume => self.should_quit = false,
            Command::NoOp => (),
        }
    }

    pub fn should_quit(&self) -> bool {
        self.should_quit
    }
}

#[cfg(test)]
mod tests {
    use super::Engine;
    use crate::command::{command_channel, Command};

    #[test]
    /// Test that applying a SetGain command updates the engine state.
    fn test_apply_command_set_gain_updates_state() {
        let mut engine = Engine::new(48_000, 440.0, 0.5);
        let mut buf = vec![0.0f32; 64];

        engine.apply_command(Command::SetGain(0.25));
        engine.render_block(&mut buf);

        // Sine is in [-1, 1]; after gain 0.25 we get [-0.25, 0.25].
        let max_abs = buf
            .iter()
            .map(|s| s.abs())
            .fold(0.0f32, |a, b| a.max(b));
        assert!(max_abs <= 0.26, "expected amplitude ~0.25, got {}", max_abs);
        assert!(max_abs > 0.0, "expected non-silent output");
    }

    #[test]
    /// Test that draining commands apply all pending commands.
    fn test_drain_commands_drains_all_pending_commands() {
        let (cmd_tx, cmd_rx) = command_channel(8);
        let mut engine = Engine::new(48_000, 440.0, 0.5);

        cmd_tx.try_send(Command::SetGain(0.1)).unwrap();
        cmd_tx.try_send(Command::SetGain(0.2)).unwrap();
        cmd_tx.try_send(Command::SetGain(0.3)).unwrap();

        engine.drain_commands(&cmd_rx);

        assert!(cmd_rx.try_recv().is_none(), "receiver should be empty after drain");
        // Last applied gain should be 0.3.
        let mut buf = vec![0.0f32; 64];
        engine.render_block(&mut buf);
        let max_abs = buf.iter().map(|s| s.abs()).fold(0.0f32, |a, b| a.max(b));
        assert!(max_abs <= 0.32 && max_abs > 0.0, "gain 0.3 should affect output");
    }

    #[test]
    /// Test that applying a Quit command sets the should_quit flag.
    fn test_apply_command_quit_sets_should_quit() {
        let mut engine = Engine::new(48_000, 440.0, 0.5);
        assert!(!engine.should_quit());

        engine.apply_command(Command::Quit);
        assert!(engine.should_quit());
    }

    #[test]
    /// Test that process_audio renders when not quit.
    fn test_process_audio_renders_when_not_quit() {
        let (_, cmd_rx) = command_channel(8);
        let mut engine = Engine::new(48_000, 440.0, 0.5);
        let mut buf = vec![0.0f32; 64];

        engine.process_audio(&cmd_rx, &mut buf);

        let max_abs = buf.iter().map(|s| s.abs()).fold(0.0f32, |a, b| a.max(b));
        assert!(max_abs > 0.0, "process_audio should produce audio when not quit");
    }

    #[test]
    /// Test that process_audio outputs silence after Quit.
    fn test_process_audio_outputs_silence_when_quit() {
        let (cmd_tx, cmd_rx) = command_channel(8);
        let mut engine = Engine::new(48_000, 440.0, 0.5);
        let mut buf = vec![1.0f32; 64]; // non-zero so we can detect overwrite

        cmd_tx.try_send(Command::Quit).unwrap();
        engine.process_audio(&cmd_rx, &mut buf);

        assert!(buf.iter().all(|&s| s == 0.0), "process_audio should output silence when quit");
    }

    #[test]
    /// Test that process_audio drains commands then renders (one-call pipeline).
    fn test_process_audio_drains_commands_then_renders() {
        let (cmd_tx, cmd_rx) = command_channel(8);
        let mut engine = Engine::new(48_000, 440.0, 0.5);
        let mut buf = vec![0.0f32; 64];

        cmd_tx.try_send(Command::SetGain(0.25)).unwrap();
        engine.process_audio(&cmd_rx, &mut buf);

        let max_abs = buf.iter().map(|s| s.abs()).fold(0.0f32, |a, b| a.max(b));
        assert!(max_abs <= 0.26 && max_abs > 0.0, "process_audio should apply enqueued SetGain");
    }
}

