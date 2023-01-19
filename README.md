# vectored-queue
Synchronized MPSC queue of IoSlice buffers to be used with write_vectored

# Disclaimer

This library is at very early stage of development (and it's my first Rust library). Many things still need to be done: more tests, proper benchmarking, nice documentation, CI/CD, etc.

# Use

```rust
use std::{io::IoSlice, sync::Arc, thread};
use vectored_queue::{AsyncVectoredQueue, VectoredQueue, SyncVectoredQueue};
// Let's suppose that https://github.com/rust-lang/rust/issues/70436 is stabilized
let write_vectored = |_io_slices: &mut [IoSlice]| todo!();

/* Bounded use case */
// Initialize the queue with a capacity
let unbounded_queue = VectoredQueue::with_capacity(42);
// Enqueue bytes, as long it implements AsRef<[u8]>
unbounded_queue.try_enqueue(b"some bytes".to_vec()).unwrap();
unbounded_queue.try_enqueue(b"more bytes".to_vec()).unwrap();
// Dequeue all enqueued in
let mut vectored = match unbounded_queue.try_dequeue_vectored().unwrap() {
    TryDequeueResult::Empty => unreachable!(),
    // it means the queue is in a transitory state,
    // waiting "current" insertions to complete
    TryDequeueResult::Pending => unreachable!(),
    TryDequeueResult::Vectored(v) => v,
};
// Vectored can be deferenced as an IoSlice buffer
write_vectored(&mut vectored);
// and it comes with the total size of the slices as a bonus
assert_eq!(vectored.total_size(), 20);
// Header and trailer can be conveniently added (without reallocation)
let header = b"header".to_vec();
let trailer = b"trailer".to_vec();
let mut frame = vectored.frame(
    ..,
    Some(IoSlice::new(&header)),
    Some(IoSlice::new(&trailer)),
);
write_vectored(&mut frame);

/* Unbounded use case */
// Queue can also be declared without capacity (allocation are done on need).
let bounded_queue = VectoredQueue::new();
// Unbounded enqueuing will increase buffer(s) capacity on need
bounded_queue
    .enqueue_unbounded(b"some bytes".to_vec())
    .unwrap();
bounded_queue
    .enqueue_unbounded(b"more bytes".to_vec())
    .unwrap();
write_vectored(
    &mut bounded_queue
        .try_dequeue_vectored()
        .unwrap()
        .vectored()
        .unwrap(),
);

/* Closing/reopening */
let queue = VectoredQueue::new();
queue.enqueue_unbounded("some bytes").unwrap();
// Close the queue, no more bytes can be enqueued
queue.close();
queue.enqueue_unbounded("more bytes").unwrap_err();
// But remaining ones can still be dequeued
bounded_queue
    .try_dequeue_vectored()
    .unwrap()
    .vectored()
    .unwrap();
bounded_queue.try_dequeue_vectored().unwrap_err();
// Queue can be reopened
queue.reopen();
queue.enqueue_unbounded("new bytes").unwrap();

/* Sync queue (feature "sync") */
let queue = Arc::new(SyncVectoredQueue::with_capacity(1));
// A new `enqueue` method allow waiting for the capacity before enqueuing
// (`enqueued_unbounded` still works)
thread::spawn({
    let q = queue.clone();
    move || q.enqueue("some_bytes")
});
// dequeuing also have its synchronous counterpart
let vectored = queue.dequeue_vectored().unwrap();

/* Async queue (feature "async") */
let queue = Arc::new(AsyncVectoredQueue::with_capacity(1));
// Again `enqueue` method allow waiting for the capacity before enqueuing
// (`enqueued_unbounded` still works synchronously)
tokio::spawn({
    let q = queue.clone();
    move || async { q.enqueue("some_bytes").await }
});
// with async dequeuing
let vectored = queue.dequeue_vectored().await.unwrap();
```

# How does it work

Internally, this queue use 2 buffers, one for enqueuing, and the other one currently dequeued. Each time `try_dequeue_vectored` is called, both buffers are swapped atomically, so dequeued buffer will contain previously enqueued bytes, and new enqueued ones will go to the other, empty buffer; in return, when the dequeued buffer, which is actually a guarded reference is dropped, it is cleared and swapped again.

Enqueuing is done by reserving atomically a "slot" on the current enqueuing buffer, basically an increasing CAS. Then the bytes are written in the slot. Actually, each buffer is divided in two: one "owned" buffer, for the bytes objects passed to try_enqueue, and one "slice" buffer, with the `IoSlice` directly written in it — "slice" buffer have in fact to more slots than the "owned" one, allowing addition of a `header`/`trailer` later. Yes, it's a self-referential pattern, so it requires a bit of unsafe code, as well as writing simultaneously in several slots of a buffer requires.

As this two-phase writing cannot be atomic, the queue can be in a transitory state, where slots have been reserved but have not been written yet. Hopefully, this issue can be solved simply by adding some synchronization primitives, like `futures::task::AtomicWaker`. `AsyncVectoredQueue` and `SyncVectoredQueue` already implement this synchronization on top of `VectoredQueue`.

Queue enqueuing can also be unbounded, with the addition of a 5th buffer, a simple `Mutex<Vec<T>>`, used when the primary enqueuing buffer is already full. Yes, this mutex can imply a high contention in this case, but it's amortized. Indeed, at the next buffer swap (dequeue operation), the capacity will be increased, with new enqueuing buffer resized, and the temporary drained into it — this insertion happens before swap, so insertion order is preserved.

Last but not least, as this algorithm is MPSC, `try_dequeue_vectored` will return an error if called while another dequeuing is happening.