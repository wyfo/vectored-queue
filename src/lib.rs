#[cfg(feature = "async")]
mod r#async;
mod buffer;
pub mod error;
mod queue;
#[cfg(feature = "sync")]
mod sync;
mod vectored;

#[cfg(feature = "async")]
pub use r#async::AsyncVectoredQueue;
#[cfg(feature = "sync")]
pub use sync::SyncVectoredQueue;

pub use crate::{
    queue::{TryDequeueResult, VectoredQueue},
    vectored::{Vectored, VectoredFrame},
};
