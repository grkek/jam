//! Global kernel message storage for local actor-to-actor delivery.
//!
//! Both `TellActor` and actor termination must use this same pool. For now
//! messages cannot carry retained capability references. Scroll transfer will
//! require replacing the `()` reference type and zero reference capacity.

use foundation::resource_budget::{
    MAXIMUM_INBOX_MESSAGE_COUNT, MAXIMUM_MESSAGE_PAYLOAD_BYTE_COUNT, MAXIMUM_POOL_MESSAGE_COUNT,
};

use super::message_pool::MessagePool;
use crate::synchronization::spin_lock::SpinLock;

const _: () = assert!(
    MAXIMUM_INBOX_MESSAGE_COUNT
        <= MAXIMUM_POOL_MESSAGE_COUNT * (MAXIMUM_MESSAGE_PAYLOAD_BYTE_COUNT + 512)
);

/// Shared message pool used by local actor delivery and cleanup.
pub static MESSAGE_POOL: SpinLock<
    MessagePool<(), MAXIMUM_INBOX_MESSAGE_COUNT, MAXIMUM_MESSAGE_PAYLOAD_BYTE_COUNT, 0>,
> = SpinLock::new(MessagePool::new());
