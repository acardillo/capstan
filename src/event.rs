//! Event: notifications from the audio thread to the control thread. No heap allocation:
//! all variants are fixed-size so they can be stored in the SPSC ring buffer.

use std::sync::Arc;

use crate::ring_buffer::RingBuffer;

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

/// Producer side of the event channel. Only the audio thread should hold this.
/// Call `try_send(event)` to enqueue an event for the control thread.
pub struct EventSender {
    inner: Arc<RingBuffer<Event>>,
}

impl EventSender {
    /// Tries to send an event. Returns `Ok(())` if enqueued, `Err(event)` if the buffer is full.
    pub fn try_send(&self, event: Event) -> Result<(), Event> {
       self.inner.try_send(event)
    }
}

/// Consumer side of the event channel. Only the control thread should hold this.
/// Call `try_recv()` to drain pending events (e.g. in the main loop).
pub struct EventReceiver {
    inner: Arc<RingBuffer<Event>>,
}

impl EventReceiver {
    /// Tries to receive the next event. Returns `None` if the buffer is empty.
    pub fn try_recv(&self) -> Option<Event> {
        self.inner.try_recv()
    }
}

/// Creates an event channel: returns a sender (for the audio thread) and a receiver (for the control thread).
pub fn event_channel(capacity: usize) -> (EventSender, EventReceiver) {
    let ring_buffer = RingBuffer::<Event>::new(capacity);
    let arc = Arc::new(ring_buffer);
    (EventSender { inner: arc.clone() }, EventReceiver { inner: arc })
}

#[cfg(test)]
mod tests {
    use super::{Event, event_channel};

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

    #[test]
    fn test_event_channel_send_recv() {
        let (sender, receiver) = event_channel(4);
        sender.try_send(Event::GraphSwapped).unwrap();
        assert_eq!(receiver.try_recv(), Some(Event::GraphSwapped));
    }
}
