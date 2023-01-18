use std::{
    collections::Bound,
    fmt,
    io::IoSlice,
    mem,
    ops::{Deref, DerefMut, RangeBounds},
};

use crate::queue::VectoredQueue;

pub struct Vectored<'a, T>
where
    T: AsRef<[u8]>,
{
    pub(crate) queue: &'a VectoredQueue<T>,
    pub(crate) buffer_index: usize,
    pub(crate) slices: &'a mut [IoSlice<'a>],
    pub(crate) total_size: usize,
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

    pub fn frame<'b>(
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
