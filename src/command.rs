//! Command: instructions from the control thread to the audio thread. No heap allocation:
//! all variants are fixed-size so they can be stored in the SPSC ring buffer.

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

#[cfg(test)]
mod tests {
    use super::Command;

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
}
