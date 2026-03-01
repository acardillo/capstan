//! Engine: runs a compiled graph or a hardcoded chain on the audio thread,
//! draining commands at the top of each callback.

use crate::audio_buffer::AudioBuffer;
use crate::command::{Command, CommandReceiver};
use crate::event::{Event, EventSender};
use crate::graph::CompiledGraph;
use crate::nodes::{GainProcessor, SineGenerator};
use crate::processor::Processor;

/// Max frames for fallback chain scratch (no allocation in callback).
const FALLBACK_SCRATCH_FRAMES: usize = 4096;

/// Engine state: optional compiled graph (when set, it is run); otherwise hardcoded sine→gain chain.
pub struct Engine {
    sine_generator: SineGenerator,
    gain_processor: GainProcessor,
    should_quit: bool,
    current_graph: Option<CompiledGraph>,
    /// Scratch for fallback chain so gain has a separate input buffer.
    fallback_scratch: AudioBuffer,
}

impl Engine {
    pub fn new(sample_rate: u32, frequency_hz: f32, initial_gain: f32) -> Self {
        Engine {
            sine_generator: SineGenerator::new(frequency_hz, sample_rate),
            gain_processor: GainProcessor::new(initial_gain),
            should_quit: false,
            current_graph: None,
            fallback_scratch: AudioBuffer::new(FALLBACK_SCRATCH_FRAMES),
        }
    }

    /// Drain all currently pending commands and apply them.
    pub fn drain_commands(&mut self, cmd_rx: &CommandReceiver, evt_tx: &EventSender) {
        while let Some(cmd) = cmd_rx.try_recv() {
            self.apply_command(cmd, evt_tx);
        }
    }

    /// Render one block: run the compiled graph if set, else the hardcoded sine→gain chain.
    pub fn render_block(&mut self, output: &mut [f32]) {
        if let Some(ref mut graph) = self.current_graph {
            graph.process(output);
        } else {
            let n = output.len().min(self.fallback_scratch.len());
            let scratch = self.fallback_scratch.as_mut_slice();
            let (scratch_n, _) = scratch.split_at_mut(n);
            self.sine_generator.process(&[], scratch_n);
            self.gain_processor.process(&[&*scratch_n], &mut output[..n]);
            if output.len() > n {
                output[n..].fill(0.0);
            }
        }
    }

    /// Full audio callback: drain commands, then either silence (if quit) or render.
    pub fn process_audio(
        &mut self,
        cmd_rx: &CommandReceiver,
        evt_tx: &EventSender,
        output: &mut [f32],
    ) {
        self.drain_commands(cmd_rx, evt_tx);
        if self.should_quit() {
            for s in output.iter_mut() {
                *s = 0.0;
            }
        } else {
            self.render_block(output);
        }
    }

    /// Apply a single command. SwapGraph sends the previous graph back via `evt_tx`.
    pub fn apply_command(&mut self, cmd: Command, evt_tx: &EventSender) {
        match cmd {
            Command::SetGain(gain) => self.gain_processor.gain = gain,
            Command::Quit => self.should_quit = true,
            Command::Resume => self.should_quit = false,
            Command::NoOp => (),
            Command::SwapGraph(new) => {
                if let Some(prev) = self.current_graph.replace(new) {
                    let _ = evt_tx.try_send(Event::GraphSwapped(prev));
                }
            }
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
    use crate::event::event_channel;

    #[test]
    fn test_apply_command_set_gain_updates_state() {
        let (evt_tx, _) = event_channel(4);
        let mut engine = Engine::new(48_000, 440.0, 0.5);
        let mut buf = vec![0.0f32; 64];

        engine.apply_command(Command::SetGain(0.25), &evt_tx);
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
    fn test_drain_commands_drains_all_pending_commands() {
        let (cmd_tx, cmd_rx) = command_channel(8);
        let (evt_tx, _) = event_channel(4);
        let mut engine = Engine::new(48_000, 440.0, 0.5);

        cmd_tx.try_send(Command::SetGain(0.1)).unwrap();
        cmd_tx.try_send(Command::SetGain(0.2)).unwrap();
        cmd_tx.try_send(Command::SetGain(0.3)).unwrap();

        engine.drain_commands(&cmd_rx, &evt_tx);

        assert!(cmd_rx.try_recv().is_none(), "receiver should be empty after drain");
        // Last applied gain should be 0.3.
        let mut buf = vec![0.0f32; 64];
        engine.render_block(&mut buf);
        let max_abs = buf.iter().map(|s| s.abs()).fold(0.0f32, |a, b| a.max(b));
        assert!(max_abs <= 0.32 && max_abs > 0.0, "gain 0.3 should affect output");
    }

    #[test]
    fn test_apply_command_quit_sets_should_quit() {
        let (evt_tx, _) = event_channel(4);
        let mut engine = Engine::new(48_000, 440.0, 0.5);
        assert!(!engine.should_quit());

        engine.apply_command(Command::Quit, &evt_tx);
        assert!(engine.should_quit());
    }

    #[test]
    fn test_process_audio_renders_when_not_quit() {
        let (_, cmd_rx) = command_channel(8);
        let (evt_tx, _) = event_channel(4);
        let mut engine = Engine::new(48_000, 440.0, 0.5);
        let mut buf = vec![0.0f32; 64];

        engine.process_audio(&cmd_rx, &evt_tx, &mut buf);

        let max_abs = buf.iter().map(|s| s.abs()).fold(0.0f32, |a, b| a.max(b));
        assert!(max_abs > 0.0, "process_audio should produce audio when not quit");
    }

    #[test]
    fn test_process_audio_outputs_silence_when_quit() {
        let (cmd_tx, cmd_rx) = command_channel(8);
        let (evt_tx, _) = event_channel(4);
        let mut engine = Engine::new(48_000, 440.0, 0.5);
        let mut buf = vec![1.0f32; 64];

        cmd_tx.try_send(Command::Quit).unwrap();
        engine.process_audio(&cmd_rx, &evt_tx, &mut buf);

        assert!(buf.iter().all(|&s| s == 0.0), "process_audio should output silence when quit");
    }

    #[test]
    fn test_process_audio_drains_commands_then_renders() {
        let (cmd_tx, cmd_rx) = command_channel(8);
        let (evt_tx, _) = event_channel(4);
        let mut engine = Engine::new(48_000, 440.0, 0.5);
        let mut buf = vec![0.0f32; 64];

        cmd_tx.try_send(Command::SetGain(0.25)).unwrap();
        engine.process_audio(&cmd_rx, &evt_tx, &mut buf);

        let max_abs = buf.iter().map(|s| s.abs()).fold(0.0f32, |a, b| a.max(b));
        assert!(max_abs <= 0.26 && max_abs > 0.0, "process_audio should apply enqueued SetGain");
    }

    #[test]
    fn test_swap_graph_runs_compiled_graph() {
        use crate::graph::{AudioGraph, GraphNode};
        use crate::nodes::{GainProcessor, SineGenerator};

        let (_cmd_rx, _) = command_channel(8);
        let (evt_tx, _) = event_channel(4);
        let mut g = AudioGraph::new();
        g.add_node(GraphNode::Sine(SineGenerator::new(440.0, 48_000)));
        g.add_node(GraphNode::Gain(GainProcessor::new(0.1)));
        g.add_edge(crate::graph::NodeId::new(0), crate::graph::NodeId::new(1));
        let compiled = g.compile(64).unwrap();

        let mut engine = Engine::new(48_000, 440.0, 0.5);
        engine.apply_command(Command::SwapGraph(compiled), &evt_tx);
        let mut buf = vec![0.0f32; 64];
        engine.render_block(&mut buf);
        let max_abs = buf.iter().map(|s| s.abs()).fold(0.0f32, |a, b| a.max(b));
        assert!(max_abs > 0.0 && max_abs <= 0.11, "compiled graph (gain 0.1) should run");
    }

    #[test]
    fn test_swap_graph_returns_old_via_event() {
        use crate::graph::{AudioGraph, GraphNode};
        use crate::nodes::{GainProcessor, SineGenerator};

        let (_cmd_tx, _cmd_rx) = command_channel(8);
        let (evt_tx, evt_rx) = event_channel(4);
        let mut engine = Engine::new(48_000, 440.0, 0.5);

        let mut g = AudioGraph::new();
        g.add_node(GraphNode::Sine(SineGenerator::new(440.0, 48_000)));
        g.add_node(GraphNode::Gain(GainProcessor::new(0.5)));
        g.add_edge(crate::graph::NodeId::new(0), crate::graph::NodeId::new(1));
        let first = g.compile(64).unwrap();
        engine.apply_command(Command::SwapGraph(first), &evt_tx);
        assert!(evt_rx.try_recv().is_none(), "first swap has no previous graph");

        let mut g2 = AudioGraph::new();
        g2.add_node(GraphNode::Sine(SineGenerator::new(880.0, 48_000)));
        g2.add_node(GraphNode::Gain(GainProcessor::new(0.25)));
        g2.add_edge(crate::graph::NodeId::new(0), crate::graph::NodeId::new(1));
        let second = g2.compile(64).unwrap();
        engine.apply_command(Command::SwapGraph(second), &evt_tx);
        let old = evt_rx.try_recv().expect("should receive previous graph");
        assert!(matches!(old, crate::event::Event::GraphSwapped(_)));
    }
}

