//! Command: instructions from the control thread to the audio thread. No heap allocation:
//! all variants are fixed-size so they can be stored in the SPSC ring buffer.

use std::sync::Arc;

use crate::ring_buffer::RingBuffer;

/// Instruction from the control thread to the audio thread.
#[derive(Debug, Clone, PartialEq)]
pub enum Command {
    /// Placeholder; remove or replace when you add real commands.
    NoOp,
    /// Control says: set gain to this value.
    SetGain(f32),
    /// Control says: quit the stream.
    Quit,
}

/// Producer side of the command channel. Only the control thread should hold this.
/// Call `try_send(cmd)` to enqueue a command for the audio thread.
pub struct CommandSender {
    inner: Arc<RingBuffer<Command>>,
}

impl CommandSender {
    /// Tries to send a command. Returns `Ok(())` if enqueued, `Err(cmd)` if the buffer is full.
    pub fn try_send(&self, cmd: Command) -> Result<(), Command> {
        self.inner.try_send(cmd)
    }
}

/// Consumer side of the command channel. Only the audio thread should hold this.
/// Call `try_recv()` at the top of each audio callback to drain pending commands.
pub struct CommandReceiver {
    inner: Arc<RingBuffer<Command>>,
}

impl CommandReceiver {
    /// Tries to receive the next command. Returns `None` if the buffer is empty.
    pub fn try_recv(&self) -> Option<Command> {
        self.inner.try_recv()
    }
}

/// Creates a command channel: returns a sender (for the control thread) and a receiver (for the audio thread).
pub fn command_channel(capacity: usize) -> (CommandSender, CommandReceiver) {
    let ring_buffer = RingBuffer::<Command>::new(capacity);
    let arc = Arc::new(ring_buffer);
    (
        CommandSender { inner: arc.clone() },
        CommandReceiver { inner: arc },
    )
}

#[cfg(test)]
mod tests {
    use super::{Command, command_channel};

    #[test]
    /// Test that command has no heap allocation (less than 16 bytes).
    fn test_command_no_heap_allocation() {
        assert!(std::mem::size_of::<Command>() <= 16, "Command must be small");
    }

    #[test]
    /// Test commands are equal if they are cloned.
    fn test_command_equality() {
        let command1 = Command::NoOp;
        let command2 = command1.clone();
        assert_eq!(command1, command2);
    }

    #[test]
    fn test_command_channel_send_recv() {
        let (sender, receiver) = command_channel(4);
        sender.try_send(Command::SetGain(0.5)).unwrap();
        assert_eq!(receiver.try_recv(), Some(Command::SetGain(0.5)));
    }
}
