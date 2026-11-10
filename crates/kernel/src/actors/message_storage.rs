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

/// One inbox is bounded by the shared pool, so its ceiling can never exceed the pool's size.
const _: () = assert!(
    MAXIMUM_INBOX_MESSAGE_COUNT <= MAXIMUM_POOL_MESSAGE_COUNT,
    "an inbox cannot be allowed more messages than the shared pool holds"
);

/// The concrete pool type the kernel uses, so the pool and the inbox charge derived from it
/// always come from the same constants.
pub type VariablePool =
    MessagePool<(), MAXIMUM_POOL_MESSAGE_COUNT, MAXIMUM_MESSAGE_PAYLOAD_BYTE_COUNT, 0>;

/// Shared message pool used by local actor delivery and cleanup.
pub static MESSAGE_POOL: SpinLock<VariablePool> = SpinLock::new(MessagePool::new());
