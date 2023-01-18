use std::{
    cell::UnsafeCell,
    hint,
    io::IoSlice,
    mem,
    mem::MaybeUninit,
    sync::atomic::{AtomicUsize, Ordering},
};

static EMPTY_SLICE: &[u8] = &[];

pub(crate) struct Buffer<T> {
    owned: UnsafeCell<Box<[MaybeUninit<T>]>>,
    slices: UnsafeCell<Box<[IoSlice<'static>]>>,
    len: AtomicUsize,
    total_size: AtomicUsize,
}

unsafe impl<T> Send for Buffer<T> {}

unsafe impl<T> Sync for Buffer<T> {}

impl<T> Default for Buffer<T> {
    fn default() -> Self {
        Self {
            owned: Default::default(),
            slices: Default::default(),
            len: Default::default(),
            total_size: Default::default(),
        }
    }
}

impl<T> Buffer<T> {
    pub(crate) fn with_capacity(capacity: usize) -> Self {
        let buffer = Buffer::default();
        if capacity > 0 {
            buffer.resize(capacity);
        }
        buffer
    }

    pub(crate) fn capacity(&self) -> usize {
        unsafe { &*self.owned.get() }.len()
    }

    pub(crate) fn len(&self) -> usize {
        self.len.load(Ordering::Relaxed)
    }

    pub(crate) fn resize(&self, capacity: usize) {
        if capacity > self.capacity() {
            let owned = (0..capacity).map(|_| MaybeUninit::uninit()).collect();
            let slices = vec![IoSlice::new(EMPTY_SLICE); capacity + 2];
            unsafe { self.owned.get().replace(owned) };
            unsafe { self.slices.get().replace(slices.into()) };
        }
    }

    pub(crate) fn get(&self, len: usize) -> Option<(&mut [IoSlice], usize)> {
        for _ in 0..100 {
            if self.len.load(Ordering::Acquire) == len {
                return Some((
                    unsafe { mem::transmute(&mut (*self.slices.get())[..len + 2]) },
                    self.total_size.load(Ordering::Acquire),
                ));
            }
            hint::spin_loop()
        }
        None
    }

    pub(crate) fn clear(&self, len: usize) {
        for i in 0..len {
            unsafe { (*self.owned.get())[i].assume_init_drop() }
        }
        self.len.store(0, Ordering::Relaxed);
        self.total_size.store(0, Ordering::Relaxed);
    }
}

impl<T> Buffer<T>
where
    T: AsRef<[u8]>,
{
    pub(crate) fn insert(&self, slot: usize, bytes: T) {
        let index = self.capacity() - slot;
        let owned_bytes = unsafe { (*self.owned.get())[index].write(bytes) };
        let slice = IoSlice::new(owned_bytes.as_ref());
        unsafe { (*self.slices.get())[index + 1] = mem::transmute(slice) };
        self.total_size.fetch_add(slice.len(), Ordering::AcqRel);
        self.len.fetch_add(1, Ordering::AcqRel);
    }
}

impl<T> Drop for Buffer<T> {
    fn drop(&mut self) {
        self.clear(self.len.load(Ordering::Relaxed));
    }
}
