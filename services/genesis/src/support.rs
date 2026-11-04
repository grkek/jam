//! Primitives shared by every scenario: failure, exit, raw system calls, scratch
//! pages, retrying sends and small assertion helpers.

use core::{
    hint::spin_loop,
    slice,
    sync::atomic::{AtomicBool, Ordering},
};

use foundation::{
    address::LocalAddress, stress::Tag, system_call::SystemCall,
    system_call_status::SystemCallStatus,
};
use runtime::inbox::{self, MessageSelector};

/// Tags used between stress actors.
pub mod tag {
    use foundation::stress::Tag;

    pub const COPY_CHECK: Tag = 10;
    pub const PING: Tag = 42;
    pub const NOISE_OTHER: Tag = 43;
    pub const COLLECT: Tag = 8;
    pub const SELF_FILL: Tag = 77;
    pub const SIZE_PROBE: Tag = 5;
    pub const REQUEST: Tag = 100;
    pub const REPLY: Tag = 101;
    pub const DEAD_LETTER: Tag = 1;
    pub const FILL: Tag = 9;
    pub const NEVER_SENT: Tag = 0xdead;
    pub const LEFTOVER: Tag = 99;
}

pub const PAGE_SIZE: usize = 4096;
/// Bytes the kernel adds in front of a payload when copying a message out.
pub const MESSAGE_HEADER_BYTES: usize = 24;
/// An inbox that accepts this many messages is effectively unbounded.
pub const UNBOUNDED_INBOX_LIMIT: u64 = 100_000;

const RETRY_LIMIT: u32 = 50_000_000;

/// Stops through an actor-local fault; never reports successful completion.
pub fn fail() -> ! {
    // SAFETY: Intentional illegal instruction tests the actor-local failure path.
    unsafe {
        core::arch::asm!("brk #0", options(noreturn));
    }
}

/// Relinquishes this actor permanently.
pub fn finish() -> ! {
    // SAFETY: FinishActor relinquishes this actor permanently and does not return.
    unsafe {
        core::arch::asm!("svc #0", in("x8") SystemCall::FinishActor.number(), options(noreturn));
    }
}

/// Faults unless `condition` holds.
pub fn require(condition: bool) {
    if !condition {
        fail();
    }
}

/// Faults unless a raw system call returned `expected`.
pub fn expect_status(actual: u64, expected: SystemCallStatus) {
    require(actual == expected.number());
}

/// `unwrap` that faults the actor instead of panicking.
pub trait OrFail<T> {
    fn or_fail(self) -> T;
}

impl<T, E> OrFail<T> for Result<T, E> {
    fn or_fail(self) -> T {
        self.unwrap_or_else(|_| fail())
    }
}

impl<T> OrFail<T> for Option<T> {
    fn or_fail(self) -> T {
        self.unwrap_or_else(|| fail())
    }
}

/// Test-only raw SystemCall for deliberately invalid pointers and scalar options.
///
/// # Safety
/// The kernel must validate every argument; callers pass invalid addresses on
/// purpose, so no buffer passed here may be relied upon to be accessed.
unsafe fn raw_system_call(number: SystemCall, arguments: [u64; 6]) -> (u64, u64) {
    let [mut status, mut value, two, three, four, five] = arguments;
    // SAFETY: Upheld by the caller; registers are the documented ABI.
    unsafe {
        core::arch::asm!("svc #0", in("x8") number.number(), inout("x0") status,
        inout("x1") value, in("x2") two, in("x3") three, in("x4") four, in("x5") five,
        options(nostack));
    }
    (status, value)
}

/// Status of a `ReceiveMessage` for the tag-10 selector into a raw buffer.
pub fn receive_into_raw_buffer(pointer: u64, length: u64) -> u64 {
    // SAFETY: The kernel must reject invalid user memory without accessing it.
    unsafe {
        raw_system_call(
            SystemCall::ReceiveMessage,
            [pointer, length, 2, 0, tag::COPY_CHECK, 0],
        )
        .0
    }
}

/// A poll must time out, and any finite timeout must be refused.
pub fn check_scalar_contracts(finite_timeout: u64) {
    // SAFETY: Scalar-only arguments.
    let (poll, finite) = unsafe {
        (
            raw_system_call(SystemCall::AwaitInbox, [2, 0, 999, 0, 0, 0]).0,
            raw_system_call(SystemCall::AwaitInbox, [0, 0, 0, 0, finite_timeout, 0]).0,
        )
    };
    if poll != SystemCallStatus::TimedOut.number() {
        fail_at::<0x104>();
    }
    if finite != SystemCallStatus::UnsupportedOperation.number() {
        fail_at::<0x105>();
    }
}

/// Cross-page buffers avoid depending on a large EL0 CPU stack.
#[repr(align(4096))]
struct Pages([u8; 2 * PAGE_SIZE]);
static mut INPUT: Pages = Pages([0; 2 * PAGE_SIZE]);
static mut OUTPUT: Pages = Pages([0; 2 * PAGE_SIZE]);
static SCRATCH_TAKEN: AtomicBool = AtomicBool::new(false);

/// The two scratch buffers, two pages each.
pub struct Scratch {
    pub input: &'static mut [u8],
    pub output: &'static mut [u8],
}

/// Hands out the scratch buffers once per actor; a second call faults.
pub fn take_scratch() -> Scratch {
    require(!SCRATCH_TAKEN.swap(true, Ordering::Relaxed));
    // SAFETY: Each isolated actor owns a private mapping of these statics, and the
    // guard above guarantees exactly one mutable reference exists per region.
    unsafe {
        Scratch {
            input: slice::from_raw_parts_mut((&raw mut INPUT.0).cast::<u8>(), 2 * PAGE_SIZE),
            output: slice::from_raw_parts_mut((&raw mut OUTPUT.0).cast::<u8>(), 2 * PAGE_SIZE),
        }
    }
}

pub fn input_address() -> usize {
    (unsafe { &raw const INPUT.0 }).cast::<u8>().addr()
}

pub fn output_address() -> usize {
    (unsafe { &raw const OUTPUT.0 }).cast::<u8>().addr()
}

pub fn current_address() -> LocalAddress {
    inbox::current_address().or_fail()
}

/// Faults with `brk #CODE`, so the kernel's fault line shows which check failed.
pub fn fail_at<const CODE: u16>() -> ! {
    // SAFETY: Intentional illegal instruction tests the actor-local failure path.
    unsafe {
        core::arch::asm!("brk #{code}", code = const CODE, options(noreturn));
    }
}

/// Selects on sender, tag and correlation.
pub fn selector(sender: LocalAddress, tag: Tag, correlation: u64) -> MessageSelector {
    MessageSelector {
        sender: Some(sender),
        tag: Some(tag),
        correlation: Some(correlation),
    }
}

/// Selects on tag only.
pub fn tagged(tag: Tag) -> MessageSelector {
    MessageSelector {
        tag: Some(tag),
        ..MessageSelector::default()
    }
}

/// Sends, retrying while the receiver's inbox or the shared pool is full.
pub fn send_retry(destination: LocalAddress, tag: Tag, correlation: u64, payload: &[u8]) {
    for _ in 0..RETRY_LIMIT {
        if inbox::tell_actor(destination, tag, correlation, payload).is_ok() {
            return;
        }
        spin_loop();
    }
    fail();
}

/// Reads the `index`th little-endian `u64` of a payload.
pub fn word(bytes: &[u8], index: usize) -> u64 {
    let start = index * 8;
    let slice = bytes.get(start..start + 8).or_fail();
    u64::from_le_bytes(slice.try_into().or_fail())
}

/// Deterministic xorshift generator.
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        Self(seed | 1)
    }

    pub fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }
}
