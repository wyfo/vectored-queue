use std::{
    cmp, fmt,
    io::IoSlice,
    mem,
    ops::{Bound, Deref, DerefMut, RangeBounds},
    sync::{
        atomic::{AtomicUsize, Ordering},
        Mutex,
    },
};

use buffer::Buffer;

use crate::error::{DequeueError, EnqueueError, TryEnqueueError};

#[cfg(feature = "async")]
pub mod r#async;
mod buffer;
pub mod error;
#[cfg(feature = "sync")]
pub mod sync;

const CLOSED_FLAG: usize = (usize::MAX >> 1) + 1;

pub struct VectoredQueue<T> {
    buffer_remain: AtomicUsize,
    pending_dequeue: AtomicUsize,
    capacity: AtomicUsize,
    buffers: [Buffer<T>; 2],
    tmp: Mutex<Vec<T>>,
}

impl<T> Default for VectoredQueue<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T> VectoredQueue<T> {
    pub fn new() -> Self {
        Self::with_capacity(0)
    }

    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            buffer_remain: AtomicUsize::new(capacity << 1),
            pending_dequeue: AtomicUsize::new(0),
            capacity: AtomicUsize::new(capacity),
            buffers: [
                Buffer::with_capacity(capacity),
                Buffer::with_capacity(capacity),
            ],
            tmp: Default::default(),
        }
    }

    fn current_buffer(&self) -> &Buffer<T> {
        &self.buffers[self.buffer_remain.load(Ordering::Relaxed) & 1]
    }

    pub fn capacity(&self) -> usize {
        self.current_buffer().capacity()
    }

    pub fn set_capacity(&self, capacity: usize) {
        let mut current_capa = self.capacity.load(Ordering::Relaxed);
        while capacity > current_capa {
            match self.capacity.compare_exchange_weak(
                current_capa,
                capacity,
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => break,
                Err(c) => current_capa = c,
            }
        }
    }

    pub fn len(&self) -> usize {
        self.current_buffer().len() + self.tmp.lock().unwrap().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn close(&self) {
        self.buffer_remain.fetch_or(CLOSED_FLAG, Ordering::Relaxed);
    }

    pub fn is_closed(&self) -> bool {
        self.buffer_remain.load(Ordering::Relaxed) & CLOSED_FLAG != 0
    }

    pub fn reopen(&self) {
        self.buffer_remain
            .fetch_and(!CLOSED_FLAG, Ordering::Relaxed);
    }
}

impl<T> VectoredQueue<T>
where
    T: AsRef<[u8]>,
{
    pub fn try_enqueue(&self, bytes: T) -> Result<(), TryEnqueueError<T>> {
        let mut buffer_remain = self.buffer_remain.load(Ordering::Relaxed);
        loop {
            if buffer_remain & CLOSED_FLAG != 0 {
                return Err(TryEnqueueError::Closed(bytes));
            }
            if buffer_remain >> 1 == 0 {
                return Err(TryEnqueueError::Full(bytes));
            }
            match self.buffer_remain.compare_exchange_weak(
                buffer_remain,
                buffer_remain - 2,
                Ordering::AcqRel,
                Ordering::Relaxed,
            ) {
                Ok(_) => break,
                Err(s) => buffer_remain = s,
            }
        }
        self.buffers[buffer_remain & 1].insert(buffer_remain >> 1, bytes);
        Ok(())
    }

    pub fn enqueue_unbounded(&self, bytes: T) -> Result<(), EnqueueError<T>> {
        let bytes = match self.try_enqueue(bytes) {
            Ok(_) => return Ok(()),
            Err(TryEnqueueError::Closed(bytes)) => return Err(EnqueueError(bytes)),
            Err(TryEnqueueError::Full(bytes)) => bytes,
        };
        let mut tmp = self.tmp.lock().unwrap();
        if self.capacity() == 0 {
            self.set_capacity(1);
            self.buffers[0].resize(1);
            self.buffers[1].resize(1);
            self.buffer_remain.store(1, Ordering::Release);
            drop(tmp);
            return self.enqueue_unbounded(bytes);
        }
        match self.try_enqueue(bytes) {
            Ok(_) => return Ok(()),
            Err(TryEnqueueError::Closed(bytes)) => return Err(EnqueueError(bytes)),
            Err(TryEnqueueError::Full(bytes)) => tmp.push(bytes),
        };
        Ok(())
    }

    pub fn try_dequeue(&self) -> Result<TryDequeueResult<T>, DequeueError> {
        let pending_dequeue = self.pending_dequeue.swap(usize::MAX, Ordering::Relaxed);
        if pending_dequeue == usize::MAX {
            return Err(DequeueError::Conflict);
        }
        let buffer_index = pending_dequeue & 1;
        let buffer = &self.buffers[buffer_index];
        let mut buffer_remain = self.buffer_remain.load(Ordering::Acquire);
        let len = if pending_dequeue >> 1 == 0 {
            assert_eq!(buffer_index, buffer_remain & 1);
            let buffer_capa = buffer.capacity();
            if (buffer_remain & !CLOSED_FLAG) >> 1 == buffer_capa {
                self.pending_dequeue
                    .store(pending_dequeue, Ordering::Relaxed);
                return if buffer_remain & CLOSED_FLAG != 0 {
                    Err(DequeueError::Closed)
                } else {
                    Ok(TryDequeueResult::Empty)
                };
            }
            let next_buffer_index = !buffer_remain & 1;
            let next_buffer = &self.buffers[next_buffer_index];
            let mut tmp = self.tmp.lock().unwrap();
            let tmp_len = tmp.len();
            let next_capa = cmp::max(
                next_buffer.capacity() + tmp_len,
                self.capacity.load(Ordering::Relaxed),
            );
            self.set_capacity(next_capa);
            next_buffer.resize(next_capa);
            for (i, bytes) in mem::take(tmp.deref_mut()).into_iter().enumerate() {
                next_buffer.insert(next_capa - i, bytes);
            }
            let next_buffer_remain = next_buffer_index | ((next_capa - tmp_len) << 1);
            while let Err(s) = self.buffer_remain.compare_exchange_weak(
                buffer_remain,
                next_buffer_remain | (buffer_remain & CLOSED_FLAG),
                Ordering::AcqRel,
                Ordering::Relaxed,
            ) {
                buffer_remain = s
            }
            buffer_capa - (buffer_remain >> 1)
        } else {
            pending_dequeue >> 1
        };
        let Some((slices, total_size)) = buffer.get(len) else {
            self.pending_dequeue.store(buffer_index | (len << 1), Ordering::Relaxed);
            return Ok(TryDequeueResult::Pending)
        };
        Ok(TryDequeueResult::Vectored(Vectored {
            queue: self,
            buffer_index,
            slices,
            total_size,
        }))
    }

    pub(crate) fn release(&self, buffer_index: usize, len: usize) {
        let buffer = &self.buffers[buffer_index];
        buffer.clear(len);
        self.pending_dequeue
            .store(!buffer_index & 1, Ordering::Relaxed);
    }
}

pub enum TryDequeueResult<'a, T>
where
    T: AsRef<[u8]>,
{
    Empty,
    Pending,
    Vectored(Vectored<'a, T>),
}

impl<T> fmt::Debug for TryDequeueResult<'_, T>
where
    T: AsRef<[u8]>,
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => f.debug_struct("TryDequeueResult::Empty").finish(),
            Self::Pending => f.debug_struct("TryDequeueResult::Pending").finish(),
            Self::Vectored(v) => f
                .debug_tuple("TryDequeueResult::Vectored")
                .field(v)
                .finish(),
        }
    }
}

impl<'a, T> TryDequeueResult<'a, T>
where
    T: AsRef<[u8]>,
{
    pub fn vectored(self) -> Option<Vectored<'a, T>> {
        match self {
            Self::Vectored(v) => Some(v),
            _ => None,
        }
    }
}

impl<'a, T> From<TryDequeueResult<'a, T>> for Option<Vectored<'a, T>>
where
    T: AsRef<[u8]>,
{
    fn from(res: TryDequeueResult<'a, T>) -> Self {
        res.vectored()
    }
}

pub struct Vectored<'a, T>
where
    T: AsRef<[u8]>,
{
    queue: &'a VectoredQueue<T>,
    buffer_index: usize,
    slices: &'a mut [IoSlice<'a>],
    total_size: usize,
}

impl<T> fmt::Debug for Vectored<'_, T>
where
    T: AsRef<[u8]>,
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Vectored")
            .field("slices", &self.slices)
            .field("total_size", &self.total_size)
            .finish()
    }
}

impl<'a, T> Deref for Vectored<'a, T>
where
    T: AsRef<[u8]>,
{
    type Target = [IoSlice<'a>];
    fn deref(&self) -> &Self::Target {
        &self.slices[1..self.slices.len() - 1]
    }
}

impl<'a, T> DerefMut for Vectored<'a, T>
where
    T: AsRef<[u8]>,
{
    fn deref_mut(&mut self) -> &mut Self::Target {
        let slices_len = self.slices.len();
        &mut self.slices[1..slices_len - 1]
    }
}

impl<'a, T> Vectored<'a, T>
where
    T: AsRef<[u8]>,
{
    pub fn total_size(&self) -> usize {
        self.total_size
    }

    pub fn frame<'b: 'a>(
        &mut self,
        range: impl RangeBounds<usize>,
        mut header: Option<IoSlice<'b>>,
        mut trailer: Option<IoSlice<'b>>,
    ) -> VectoredFrame<'b> {
        let mut start = match range.start_bound() {
            Bound::Included(&n) => n,
            Bound::Excluded(&n) => n + 1,
            Bound::Unbounded => 0,
        };
        let mut end = match range.end_bound() {
            Bound::Included(&n) => n + 2,
            Bound::Excluded(&n) => n + 1,
            Bound::Unbounded => self.slices.len(),
        };
        let slices: &'b mut [IoSlice<'b>] = unsafe { mem::transmute(&mut *(self.slices)) };
        if let Some(ref mut header) = header {
            mem::swap(header, &mut slices[start]);
        } else {
            start += 1;
        }
        if let Some(ref mut trailer) = trailer {
            mem::swap(trailer, &mut slices[end - 1]);
        } else {
            end -= 1;
        }
        VectoredFrame {
            slices: &mut slices[start..end],
            header,
            trailer,
        }
    }
}

impl<'a, T> Drop for Vectored<'a, T>
where
    T: AsRef<[u8]>,
{
    fn drop(&mut self) {
        self.queue.release(self.buffer_index, self.slices.len() - 2);
    }
}

pub struct VectoredFrame<'a> {
    slices: &'a mut [IoSlice<'a>],
    header: Option<IoSlice<'a>>,
    trailer: Option<IoSlice<'a>>,
}

impl fmt::Debug for VectoredFrame<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("VectoredFrame").field(&self.slices).finish()
    }
}

impl<'a> Deref for VectoredFrame<'a> {
    type Target = [IoSlice<'a>];
    fn deref(&self) -> &Self::Target {
        self.slices
    }
}

impl<'a> DerefMut for VectoredFrame<'a> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.slices
    }
}

impl<'a> Drop for VectoredFrame<'a> {
    fn drop(&mut self) {
        if let Some(header) = self.header {
            self.slices[0] = header
        }
        if let Some(trailer) = self.trailer {
            self.slices[self.slices.len() - 1] = trailer
        }
    }
}

#[cfg(test)]
mod test {
    use std::ops::Deref;

    use crate::{error::DequeueError, Vectored, VectoredQueue};

    #[test]
    fn it_works() {
        let queue = VectoredQueue::with_capacity(4);
        // let b1 = vec![0, 1].into_boxed_slice();
        let b2 = vec![2].into_boxed_slice();
        let b3 = vec![3].into_boxed_slice();
        let b4 = vec![4, 5].into_boxed_slice();
        let collect = |vectored: &Vectored<Box<[u8]>>| {
            vectored
                .iter()
                .flat_map(|s| s.deref().iter().cloned())
                .collect::<Vec<_>>()
        };
        // queue.try_enqueue(b1).unwrap();
        queue.try_enqueue(b2).unwrap();
        let vectored = queue.try_dequeue().unwrap().vectored().unwrap();
        queue.try_enqueue(b3).unwrap();
        assert_eq!(vectored.total_size(), 1);
        assert_eq!(collect(&vectored), vec![2]);
        // assert_eq!(vectored.total_size(), 3);
        // assert_eq!(collect(&vectored), vec![0, 1, 2]);
        assert!(matches!(queue.try_dequeue(), Err(DequeueError::Conflict)));
        drop(vectored);
        queue.try_enqueue(b4).unwrap();
        let vectored = queue.try_dequeue().unwrap().vectored().unwrap();
        assert_eq!(vectored.total_size(), 3);
        assert_eq!(collect(&vectored), vec![3, 4, 5]);
    }
}