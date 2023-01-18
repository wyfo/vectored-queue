#[derive(Debug, Copy, Clone, Eq, PartialEq, thiserror::Error)]
pub enum TryEnqueueError<T> {
    #[error("queue is full")]
    Full(T),
    #[error("queue is closed")]
    Closed(T),
}

impl<T> TryEnqueueError<T> {
    pub fn inner(self) -> T {
        match self {
            Self::Full(v) | Self::Closed(v) => v,
        }
    }
}

#[derive(Debug, Copy, Clone, Eq, PartialEq, thiserror::Error)]
#[error("queue is closed")]
pub struct EnqueueError<T>(pub T);

impl<T> EnqueueError<T> {
    pub fn inner(self) -> T {
        self.0
    }
}

#[derive(Debug, Copy, Clone, Eq, PartialEq, thiserror::Error)]
pub enum DequeueError {
    #[error("queue is closed")]
    Closed,
    #[error("queue is dequeued in another thread")]
    Conflict,
}
