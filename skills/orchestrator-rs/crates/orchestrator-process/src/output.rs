use std::{
    fmt,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
        mpsc::{Receiver, RecvTimeoutError, SyncSender, TrySendError, sync_channel},
    },
    time::Duration,
};

// Only the pipe readers can enqueue, and each read is bounded by READER_CHUNK.
const OUTPUT_QUEUE_CHUNKS: usize = 16;

/// Identifies the pipe that produced one diagnostic chunk.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProcessOutputStream {
    Stdout,
    Stderr,
}

/// Best-effort live bytes. This is not a terminal process receipt.
pub struct ProcessOutputChunk {
    /// Pipe that produced these bytes; ordering across pipes is unspecified.
    pub stream: ProcessOutputStream,
    /// One bounded read; a chunk may split a UTF-8 character or protocol line.
    pub bytes: Vec<u8>,
}

impl fmt::Debug for ProcessOutputChunk {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProcessOutputChunk")
            .field("stream", &self.stream)
            .field("byte_count", &self.bytes.len())
            .finish()
    }
}

#[derive(Clone)]
/// Non-blocking diagnostic delivery handle with a fixed queue capacity.
pub struct ProcessOutputSender {
    sender: SyncSender<ProcessOutputChunk>,
    dropped_bytes: Arc<AtomicU64>,
}

impl ProcessOutputSender {
    pub(crate) fn try_send(&self, chunk: ProcessOutputChunk) {
        if let Err(error) = self.sender.try_send(chunk) {
            let chunk = match error {
                TrySendError::Full(chunk) | TrySendError::Disconnected(chunk) => chunk,
            };
            let dropped = u64::try_from(chunk.bytes.len()).unwrap_or(u64::MAX);
            let _ =
                self.dropped_bytes
                    .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
                        Some(current.saturating_add(dropped))
                    });
        }
    }
}

impl fmt::Debug for ProcessOutputSender {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProcessOutputSender")
            .field("dropped_bytes", &self.dropped_bytes.load(Ordering::Relaxed))
            .finish_non_exhaustive()
    }
}

/// Bounded diagnostic receiver. Dropping it does not cancel the process.
pub struct ProcessOutputReceiver {
    receiver: Receiver<ProcessOutputChunk>,
    dropped_bytes: Arc<AtomicU64>,
}

impl ProcessOutputReceiver {
    /// Waits at most `timeout` for the next available chunk.
    pub fn recv_timeout(&self, timeout: Duration) -> Result<ProcessOutputChunk, RecvTimeoutError> {
        self.receiver.recv_timeout(timeout)
    }

    #[must_use]
    /// Number of bytes discarded because the delivery queue was full or closed.
    pub fn dropped_bytes(&self) -> u64 {
        self.dropped_bytes.load(Ordering::Relaxed)
    }
}

impl fmt::Debug for ProcessOutputReceiver {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProcessOutputReceiver")
            .field("dropped_bytes", &self.dropped_bytes())
            .finish_non_exhaustive()
    }
}

impl Drop for ProcessOutputReceiver {
    fn drop(&mut self) {
        let dropped = self.receiver.try_iter().fold(0u64, |total, chunk| {
            total.saturating_add(u64::try_from(chunk.bytes.len()).unwrap_or(u64::MAX))
        });
        let _ = self
            .dropped_bytes
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
                Some(current.saturating_add(dropped))
            });
    }
}

#[must_use]
/// Creates a queue of at most sixteen pipe-read chunks. Slow viewers cannot
/// block pipe draining; exact captured output remains in the process report.
pub fn output_channel() -> (ProcessOutputSender, ProcessOutputReceiver) {
    let (sender, receiver) = sync_channel(OUTPUT_QUEUE_CHUNKS);
    let dropped_bytes = Arc::new(AtomicU64::new(0));
    (
        ProcessOutputSender {
            sender,
            dropped_bytes: Arc::clone(&dropped_bytes),
        },
        ProcessOutputReceiver {
            receiver,
            dropped_bytes,
        },
    )
}
