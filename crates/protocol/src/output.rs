//! Bounded, cancellable output between blocking protocol readers and the UI.

use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
    mpsc::{self, Receiver, SendError, SyncSender, TrySendError},
};
use std::time::Duration;

use shell_core::ProtocolEvent;

// At most 1 MiB of terminal output waits for the UI per connection.
const OUTPUT_CHUNK_BYTES: usize = 8192;
const OUTPUT_QUEUE_CAPACITY: usize = 128;

pub(crate) struct OutputSender {
    sender: SyncSender<ProtocolEvent>,
    cancelled: Arc<AtomicBool>,
}

pub(crate) fn output_channel(
    cancelled: Arc<AtomicBool>,
) -> (OutputSender, Receiver<ProtocolEvent>) {
    channel_with_capacity(cancelled, OUTPUT_QUEUE_CAPACITY)
}

fn channel_with_capacity(
    cancelled: Arc<AtomicBool>,
    capacity: usize,
) -> (OutputSender, Receiver<ProtocolEvent>) {
    let (sender, receiver) = mpsc::sync_channel(capacity);
    (OutputSender { sender, cancelled }, receiver)
}

impl OutputSender {
    pub(crate) fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Relaxed)
    }

    pub(crate) fn send(&self, event: ProtocolEvent) -> Result<(), SendError<ProtocolEvent>> {
        if let ProtocolEvent::Output(bytes) = &event
            && bytes.len() > OUTPUT_CHUNK_BYTES
        {
            for chunk in bytes.chunks(OUTPUT_CHUNK_BYTES) {
                self.send_one(ProtocolEvent::Output(chunk.to_vec()))?;
            }
            return Ok(());
        }
        self.send_one(event)
    }

    fn send_one(&self, mut event: ProtocolEvent) -> Result<(), SendError<ProtocolEvent>> {
        loop {
            if self.is_cancelled() {
                return Err(SendError(event));
            }
            match self.sender.try_send(event) {
                Ok(()) => return Ok(()),
                Err(TrySendError::Disconnected(event)) => return Err(SendError(event)),
                Err(TrySendError::Full(returned)) => {
                    event = returned;
                    // A blocking SyncSender::send would deadlock when the UI
                    // closes a tab and joins its reader while still holding the
                    // receiver. Retry without spinning and observe shutdown.
                    std::thread::park_timeout(Duration::from_millis(5));
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backpressure_preserves_order_when_the_receiver_resumes() {
        let (sender, receiver) = channel_with_capacity(Arc::new(AtomicBool::new(false)), 1);
        sender.send(ProtocolEvent::Output(vec![1])).unwrap();
        let (done_tx, done_rx) = mpsc::channel();
        let worker = std::thread::spawn(move || {
            sender.send(ProtocolEvent::Output(vec![2])).unwrap();
            done_tx.send(()).unwrap();
        });
        assert!(done_rx.recv_timeout(Duration::from_millis(50)).is_err());
        assert_eq!(receiver.recv().unwrap(), ProtocolEvent::Output(vec![1]));
        done_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        assert_eq!(receiver.recv().unwrap(), ProtocolEvent::Output(vec![2]));
        worker.join().unwrap();
    }

    #[test]
    fn shutdown_unblocks_a_full_queue_even_when_receiver_is_still_alive() {
        let cancelled = Arc::new(AtomicBool::new(false));
        let (sender, receiver) = channel_with_capacity(Arc::clone(&cancelled), 1);
        sender.send(ProtocolEvent::Output(vec![1])).unwrap();
        let (done_tx, done_rx) = mpsc::channel();
        let worker = std::thread::spawn(move || {
            done_tx
                .send(sender.send(ProtocolEvent::Output(vec![2])).is_err())
                .unwrap();
        });
        assert!(done_rx.recv_timeout(Duration::from_millis(50)).is_err());
        cancelled.store(true, Ordering::Relaxed);
        assert!(done_rx.recv_timeout(Duration::from_secs(2)).unwrap());
        worker.join().unwrap();
        assert_eq!(receiver.recv().unwrap(), ProtocolEvent::Output(vec![1]));
        assert!(receiver.recv().is_err());
    }

    #[test]
    fn receiver_disconnect_unblocks_a_full_queue() {
        let (sender, receiver) = channel_with_capacity(Arc::new(AtomicBool::new(false)), 1);
        sender.send(ProtocolEvent::Output(vec![1])).unwrap();
        let (done_tx, done_rx) = mpsc::channel();
        let worker = std::thread::spawn(move || {
            done_tx
                .send(sender.send(ProtocolEvent::Exited(None)).is_err())
                .unwrap();
        });
        drop(receiver);
        assert!(done_rx.recv_timeout(Duration::from_secs(2)).unwrap());
        worker.join().unwrap();
    }

    #[test]
    fn oversized_output_is_split_into_bounded_frames_without_losing_bytes() {
        let (sender, receiver) = output_channel(Arc::new(AtomicBool::new(false)));
        let bytes: Vec<_> = (0..OUTPUT_CHUNK_BYTES * 3 + 7).map(|n| n as u8).collect();
        sender.send(ProtocolEvent::Output(bytes.clone())).unwrap();
        drop(sender);
        let mut received = Vec::new();
        for event in receiver {
            let ProtocolEvent::Output(chunk) = event else {
                panic!("unexpected event")
            };
            assert!(chunk.len() <= OUTPUT_CHUNK_BYTES);
            received.extend_from_slice(&chunk);
        }
        assert_eq!(received, bytes);
    }
}
