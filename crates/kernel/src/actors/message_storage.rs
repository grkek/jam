//! Global kernel message storage for local actor-to-actor delivery.
//!
//! Both `TellActor` and actor termination must use this same pool. For now
//! messages cannot carry retained capability references. Scroll transfer will
//! require replacing the `()` reference type and zero reference capacity.

use super::message_pool::MessagePool;
use crate::synchronization::spin_lock::SpinLock;

/// Maximum number of simultaneously allocated kernel messages.
pub const MESSAGE_POOL_CAPACITY: usize = 128;

/// Maximum payload bytes in one kernel-owned message.
pub const MESSAGE_MAXIMUM_PAYLOAD_BYTES: usize = 4096;

/// Maximum number of messages one actor's inbox may hold at once, including a
/// message currently leased for copy-out. Bounds per-actor descriptor storage
/// and the O(n) scans in selective receive.
pub const ACTOR_INBOX_CAPACITY: usize = 64;

/// Maximum resident bytes one actor's inbox may be charged, in the same units
/// as `MessageCharge` (message-pool slot plus envelope per message). A second
/// quota alongside `ACTOR_INBOX_CAPACITY`: whichever limit is reached first
/// rejects further sends with `SendError::BudgetExceeded` or `Full`.
pub const ACTOR_INBOX_BYTE_BUDGET: usize = 288 * 1024;

const _: () = assert!(
    ACTOR_INBOX_BYTE_BUDGET <= MESSAGE_POOL_CAPACITY * (MESSAGE_MAXIMUM_PAYLOAD_BYTES + 512)
);

/// Shared message pool used by local actor delivery and cleanup.
pub static MESSAGE_POOL: SpinLock<
    MessagePool<(), MESSAGE_POOL_CAPACITY, MESSAGE_MAXIMUM_PAYLOAD_BYTES, 0>,
> = SpinLock::new(MessagePool::new());
