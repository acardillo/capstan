//! SPSC (single producer, single consumer) ring buffer for lock-free messaging
//! between the control thread and the audio thread

use std::mem::MaybeUninit;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::ptr;

/// Lock-free SPSC ring buffer. One thread may call `try_send`; another may call `try_recv`.
/// No allocation in send/recv; capacity fixed at creation.
pub struct RingBuffer<T> {
    /// Slots (uninitialized until sent, then read by consumer)
    storage: Box<[MaybeUninit<T>]>,
    /// Capacity (storage.len()). Must be > 0.
    cap: usize,
    /// Producer index: next slot to write. Consumer never writes this.
    write_index: AtomicUsize,
    /// Consumer index: next slot to read. Producer never writes this.
    read_index: AtomicUsize,
} 

impl<T> RingBuffer<T> {
    /// Creates a ring buffer with the given capacity. No allocation after this.
    pub fn new(capacity: usize) -> Self {
        assert!(capacity > 0, "capacity must be > 0");
        let storage: Box<[MaybeUninit<T>]> = Box::new_uninit_slice(capacity);
        RingBuffer { 
            storage,
            cap: capacity, 
            write_index: AtomicUsize::new(0), 
            read_index: AtomicUsize::new(0) 
        }
    }

    /// Tries to send a value. Returns `Ok(())` if enqueued, `Err(value)` if full.
    /// Only the producer thread may call this.
    pub fn try_send(&self, value: T) -> Result<(), T> {
        let read = self.read_index.load(Ordering::Acquire);
        let write = self.write_index.load(Ordering::Relaxed);

        // Full if number of items == cap.
        if write.wrapping_sub(read) == self.cap {
            return Err(value);
        }

        // Compute slot index in the ring.
        let index = write % self.cap;

        // SAFETY: only the producer thread writes this slot, and we only write
        // when the ring is not full, so we don't overwrite an unread value.
        unsafe {
            let ptr = self.storage.as_ptr().add(index) as *mut MaybeUninit<T>;
            ptr::write(ptr, MaybeUninit::new(value));
        }

        self.write_index.store(write.wrapping_add(1), Ordering::Release);
        Ok(())
    }

    /// Tries to receive a value. Returns `Some(value)` if one was enqueued, `None` if empty.
    /// Only the consumer thread may call this.
    pub fn try_recv(&self) -> Option<T> {
        let read = self.read_index.load(Ordering::Relaxed);
        let write = self.write_index.load(Ordering::Acquire);

        // Empty if read_index == write_index.
        if read == write {
            return None;
        }

        // Compute slot index in the ring.
        let index = read % self.cap;

        // SAFETY: only the consumer thread reads this slot, and we only read
        // when the ring is not empty, so we don't read an unwritten value.
        unsafe {
            let ptr = self.storage.as_ptr().add(index) as *mut MaybeUninit<T>;
            let value = ptr::read(ptr);

            self.read_index.store(read.wrapping_add(1), Ordering::Release);
            Some(value.assume_init())
        }
    }

    /// Returns whether the buffer is empty (nothing to recv).
    pub fn is_empty(&self) -> bool {
        let read = self.read_index.load(Ordering::Relaxed);
        let write = self.write_index.load(Ordering::Relaxed);

        let is_empty = read == write;
        is_empty
    }

    /// Returns whether the buffer is full (try_send would fail).
    pub fn is_full(&self) -> bool {
        let write = self.write_index.load(Ordering::Relaxed);
        let read = self.read_index.load(Ordering::Relaxed);

        let is_full = write.wrapping_sub(read) == self.cap;
        is_full
    }
}

#[cfg(test)]
mod tests {
    use super::RingBuffer;

    #[test]
    /// Test that sending a value and then receiving it returns the value.
    fn test_send_then_recv_returns_value() {
        let ring_buffer: RingBuffer<i32> = RingBuffer::new(1);
        ring_buffer.try_send(42).unwrap();
        assert_eq!(ring_buffer.try_recv(), Some(42));
    }

    #[test]
    /// Test that receiving from an empty buffer returns None.
    fn test_empty_recv_returns_none() {
        let ring_buffer: RingBuffer<i32> = RingBuffer::new(1);
        assert_eq!(ring_buffer.try_recv(), None); 
    }

    #[test]
    /// Test that sending a value to a full buffer returns Err.
    fn test_full_send_returns_err() {
        let ring_buffer: RingBuffer<i32> = RingBuffer::new(1);
        ring_buffer.try_send(42).unwrap();
        assert_eq!(ring_buffer.try_send(43), Err(43));
    }

    #[test]
    /// Test that the values are received in the order they were sent.
    fn test_fifo_order() {
        let ring_buffer: RingBuffer<i32> = RingBuffer::new(3);
        ring_buffer.try_send(1).unwrap();
        ring_buffer.try_send(2).unwrap();
        ring_buffer.try_send(3).unwrap();
        assert_eq!(ring_buffer.try_recv(), Some(1));
        assert_eq!(ring_buffer.try_recv(), Some(2));
        assert_eq!(ring_buffer.try_recv(), Some(3));
    }
}
