//! Event: notifications from the audio thread to the control thread. No heap allocation:
//! all variants are fixed-size so they can be stored in the SPSC ring buffer.

/// Notification from the audio thread to the control thread.
#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    /// Placeholder; remove or replace when you add real events.
    NoOp,
    /// Audio thread applied a new compiled graph; control thread may drop the previous one.
    GraphSwapped,
    /// Audio thread stopped the stream; control thread may restart it.
    StreamStopped,
}

#[cfg(test)]
mod tests {
    use super::Event;

    #[test]
    fn test_event_no_heap_allocation() {
        assert!(std::mem::size_of::<Event>() <= 16, "Event must be small");
    }

    #[test]
    fn test_event_equality() {
        let event1 = Event::NoOp;
        let event2 = event1.clone();
        assert_eq!(event1, event2);
    }
}
