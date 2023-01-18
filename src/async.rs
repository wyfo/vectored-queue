use std::task::Poll;

use futures::task::AtomicWaker;

use crate::{
    error::{DequeueError, EnqueueError, TryEnqueueError},
    TryDequeueResult, Vectored, VectoredQueue,
};

pub struct AsyncVectoredQueue<T> {
    queue: VectoredQueue<T>,
    waker: AtomicWaker,
    notify: tokio::sync::Notify,
}

impl<T> Default for AsyncVectoredQueue<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T> AsyncVectoredQueue<T> {
    pub fn new() -> Self {
        Self::with_capacity(0)
    }

    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            queue: VectoredQueue::with_capacity(capacity),
            waker: AtomicWaker::default(),
            notify: tokio::sync::Notify::new(),
        }
    }

    pub fn capacity(&self) -> usize {
        self.queue.capacity()
    }

    pub fn set_capacity(&self, capacity: usize) {
        self.queue.set_capacity(capacity);
    }

    pub fn len(&self) -> usize {
        self.queue.len()
    }

    pub fn is_empty(&self) -> bool {
        self.queue.is_empty()
    }

    pub fn close(&self) {
        self.queue.close();
        self.notify.notify_waiters();
    }

    pub fn is_closed(&self) -> bool {
        self.queue.is_closed()
    }

    pub fn reopen(&self) {
        self.queue.reopen();
    }
}

impl<T> AsyncVectoredQueue<T>
where
    T: AsRef<[u8]>,
{
    pub fn try_enqueue(&self, bytes: T) -> Result<(), TryEnqueueError<T>> {
        self.queue.try_enqueue(bytes)?;
        self.waker.wake();
        Ok(())
    }

    pub async fn enqueue(&self, mut bytes: T) -> Result<(), EnqueueError<T>> {
        loop {
            let notified = self.notify.notified();
            match self.try_enqueue(bytes) {
                Ok(_) => return Ok(()),
                Err(TryEnqueueError::Closed(bytes)) => return Err(EnqueueError(bytes)),
                Err(TryEnqueueError::Full(b)) => bytes = b,
            };
            notified.await;
        }
    }

    pub fn enqueue_unbounded(&self, bytes: T) -> Result<(), EnqueueError<T>> {
        self.queue.enqueue_unbounded(bytes)?;
        self.waker.wake();
        Ok(())
    }

    pub fn try_dequeue(&self) -> Result<TryDequeueResult<T>, DequeueError> {
        let res = self.queue.try_dequeue()?;
        if matches!(res, TryDequeueResult::Vectored(_)) {
            self.notify.notify_waiters();
        }
        Ok(res)
    }

    pub async fn dequeue(&self) -> Result<Vectored<T>, DequeueError> {
        futures::future::poll_fn(|cx| {
            if let Some(vectored) = self.try_dequeue()?.vectored() {
                return Poll::Ready(Ok(vectored));
            }
            self.waker.register(cx.waker());
            if let Some(vectored) = self.try_dequeue()?.vectored() {
                return Poll::Ready(Ok(vectored));
            }
            Poll::Pending
        })
        .await
    }
}
