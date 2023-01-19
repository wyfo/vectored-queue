use std::{
    sync::{
        atomic::{AtomicBool, Ordering},
        Condvar, Mutex,
    },
    time::Duration,
};

use crate::{
    error::{DequeueError, EnqueueError, TryEnqueueError},
    queue::{TryDequeueResult, VectoredQueue},
    vectored::Vectored,
};

pub struct SyncVectoredQueue<T> {
    queue: VectoredQueue<T>,
    cond_var: Condvar,
    lock: Mutex<()>,
    wait_dequeue: AtomicBool,
}

impl<T> Default for SyncVectoredQueue<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T> SyncVectoredQueue<T> {
    pub fn new() -> Self {
        Self::with_capacity(0)
    }

    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            queue: VectoredQueue::with_capacity(capacity),
            cond_var: Condvar::new(),
            lock: Mutex::new(()),
            wait_dequeue: AtomicBool::new(true),
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
        self.cond_var.notify_all()
    }

    pub fn is_closed(&self) -> bool {
        self.queue.is_closed()
    }

    pub fn reopen(&self) {
        self.queue.reopen();
    }
}

impl<T> SyncVectoredQueue<T>
where
    T: AsRef<[u8]>,
{
    fn wake_dequeue(&self) {
        if self.wait_dequeue.swap(false, Ordering::Relaxed) {
            self.cond_var.notify_all();
        }
    }

    pub fn try_enqueue(&self, bytes: T) -> Result<(), TryEnqueueError<T>> {
        self.queue.try_enqueue(bytes)?;
        self.wake_dequeue();
        Ok(())
    }

    fn enqueue_wait(
        &self,
        mut bytes: T,
        timeout: Option<Duration>,
    ) -> Result<(), TryEnqueueError<T>> {
        match self.try_enqueue(bytes) {
            Err(TryEnqueueError::Full(b)) => bytes = b,
            res => return res,
        };
        let mut lock = self.lock.lock().unwrap();
        loop {
            match self.try_enqueue(bytes) {
                Err(TryEnqueueError::Full(b)) => bytes = b,
                res => return res,
            };
            lock = if let Some(timeout) = timeout {
                match self.cond_var.wait_timeout(lock, timeout).unwrap() {
                    (_, timeout_res) if timeout_res.timed_out() => {
                        return Err(TryEnqueueError::Full(bytes))
                    }
                    (lock, _) => lock,
                }
            } else {
                self.cond_var.wait(lock).unwrap()
            };
        }
    }

    pub fn try_enqueue_timeout(
        &self,
        bytes: T,
        timeout: Duration,
    ) -> Result<(), TryEnqueueError<T>> {
        self.enqueue_wait(bytes, Some(timeout))
    }

    pub fn enqueue(&self, bytes: T) -> Result<(), EnqueueError<T>> {
        match self.enqueue_wait(bytes, None) {
            Ok(_) => Ok(()),
            Err(TryEnqueueError::Closed(bytes)) => Err(EnqueueError(bytes)),
            Err(TryEnqueueError::Full(_)) => unreachable!(),
        }
    }

    pub fn enqueue_unbounded(&self, bytes: T) -> Result<(), EnqueueError<T>> {
        self.queue.enqueue_unbounded(bytes)?;
        self.wake_dequeue();
        Ok(())
    }

    pub fn try_dequeue_vectored(&self) -> Result<TryDequeueResult<T>, DequeueError> {
        let res = self.queue.try_dequeue_vectored()?;
        if matches!(res, TryDequeueResult::Vectored(_)) {
            self.cond_var.notify_all();
        }
        Ok(res)
    }

    fn dequeue_vectored_wait(
        &self,
        timeout: Option<Duration>,
    ) -> Result<TryDequeueResult<T>, DequeueError> {
        let mut lock = self.lock.lock().unwrap();
        loop {
            self.wait_dequeue.store(true, Ordering::Relaxed);
            let res = self.try_dequeue_vectored()?;
            if matches!(res, TryDequeueResult::Vectored(_)) {
                return Ok(res);
            }
            lock = if let Some(timeout) = timeout {
                match self.cond_var.wait_timeout(lock, timeout).unwrap() {
                    (_, timeout_res) if timeout_res.timed_out() => return Ok(res),
                    (lock, _) => lock,
                }
            } else {
                self.cond_var.wait(lock).unwrap()
            };
        }
    }

    pub fn try_dequeue_vectored_timeout(
        &self,
        timeout: Duration,
    ) -> Result<TryDequeueResult<T>, DequeueError> {
        self.dequeue_vectored_wait(Some(timeout))
    }

    pub fn dequeue_vectored(&self) -> Result<Vectored<T>, DequeueError> {
        Ok(self.dequeue_vectored_wait(None)?.vectored().unwrap())
    }
}
