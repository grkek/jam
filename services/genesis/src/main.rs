//! Two-actor EL0 messaging demo with checked copies and selective request/reply.
//!
//! Boot must start two instances and send each a tag-zero bootstrap message:
//! little-endian packed peer address followed by a u64 role (0 or 1).
//! Each actor performs copy validation and 128 request/reply exchanges, then exits.

#![no_std]
#![no_main]

use foundation::{
    address::LocalAddress, system_call::SystemCall, system_call_status::SystemCallStatus,
};
use runtime::inbox::{self, MessageSelector};

/// Cross-page input and output avoid depending on a large EL0 CPU stack.
#[repr(align(4096))]
struct Pages([u8; 8192]);
static mut INPUT: Pages = Pages([0; 8192]);
static mut OUTPUT: Pages = Pages([0; 8192]);

/// Stops through an actor-local fault; never reports successful completion.
fn fail() -> ! {
    // SAFETY: Intentional illegal instruction tests the actor-local failure path.
    unsafe {
        core::arch::asm!("brk #0", options(noreturn));
    }
}

/// Test-only raw syscall for deliberately invalid pointers and scalar options.
unsafe fn call(number: SystemCall, arguments: [u64; 6]) -> (u64, u64) {
    let [mut status, mut value, two, three, four, five] = arguments;
    // SAFETY: This test intentionally submits invalid user addresses; the kernel
    // must validate them before access. Buffers used in successful calls are live.
    unsafe {
        core::arch::asm!("svc #0", in("x8") number.number(), inout("x0") status,
        inout("x1") value, in("x2") two, in("x3") three, in("x4") four, in("x5") five,
        options(nostack));
    }
    (status, value)
}

/// Runs copy validation followed by repeated request/reply between two actors.
pub fn run() -> ! {
    let mut record = [0u8; 128];
    let bootstrap = inbox::await_matching(
        MessageSelector {
            tag: Some(0),
            ..MessageSelector::default()
        },
        &mut record,
    )
    .unwrap_or_else(|_| fail());

    let packed = u64::from_le_bytes(
        bootstrap
            .payload
            .get(..8)
            .unwrap_or_else(|| fail())
            .try_into()
            .unwrap_or_else(|_| fail()),
    );

    let role = u64::from_le_bytes(
        bootstrap
            .payload
            .get(8..16)
            .unwrap_or_else(|| fail())
            .try_into()
            .unwrap_or_else(|_| fail()),
    );

    let peer = LocalAddress::new(
        u32::try_from(packed & u64::from(u32::MAX)).unwrap_or_else(|_| fail()),
        u32::try_from(packed >> 32).unwrap_or_else(|_| fail()),
    );

    let own = inbox::current_address().unwrap_or_else(|_| fail());
    // SAFETY: Each isolated actor owns a private mapping of these statics. This
    // single-threaded test creates exactly one mutable reference to each region.
    let (input, output) = unsafe {
        let input = core::slice::from_raw_parts_mut((&raw mut INPUT.0).cast::<u8>().add(4090), 32);
        let output =
            core::slice::from_raw_parts_mut((&raw mut OUTPUT.0).cast::<u8>().add(4090), 64);
        (input, output)
    };

    input.fill(0x5a);
    inbox::tell_actor(own, 10, 99, input).unwrap_or_else(|_| fail());

    let own_selector = MessageSelector {
        sender: Some(own),
        tag: Some(10),
        correlation: Some(99),
    };

    let small = inbox::receive_message(own_selector, &mut []).unwrap_err();
    if small.status != SystemCallStatus::BufferTooSmall || small.required_byte_count != 56 {
        fail();
    }

    // SAFETY: Deliberately invalid output pointer must be rejected without consuming.
    let invalid = unsafe { call(SystemCall::ReceiveMessage, [u64::MAX - 20, 56, 2, 0, 10, 0]) };
    if invalid.0 != SystemCallStatus::InvalidUserMemory.number() {
        fail();
    }

    let message = inbox::receive_message(own_selector, output).unwrap_or_else(|_| fail());
    if message.payload != input || message.sender != own {
        fail();
    }

    // SAFETY: Poll and unsupported timeout checks use scalar-only arguments.
    let (poll, finite) = unsafe {
        (
            call(SystemCall::AwaitInbox, [2, 0, 999, 0, 0, 0]),
            call(SystemCall::AwaitInbox, [0, 0, 0, 0, 1, 0]),
        )
    };

    if poll.0 != SystemCallStatus::TimedOut.number()
        || finite.0 != SystemCallStatus::UnsupportedOperation.number()
    {
        fail();
    }

    // This unmatched message must survive all selective exchanges and be drained on exit.
    inbox::tell_actor(own, 99, 0, b"left queued for exit cleanup").unwrap_or_else(|_| fail());

    for sequence in 0..128u64 {
        let selector = MessageSelector {
            sender: Some(peer),
            tag: Some(42),
            correlation: Some(sequence),
        };
        if role == 0 {
            inbox::tell_actor(peer, 42, sequence, b"ping").unwrap_or_else(|_| fail());
        }
        let message = inbox::await_matching(selector, &mut record).unwrap_or_else(|_| fail());
        let expected: &[u8] = if role == 0 { b"pong" } else { b"ping" };
        if message.sender != peer || message.payload != expected {
            fail();
        }
        if role == 1 {
            inbox::tell_actor(peer, 42, sequence, b"pong").unwrap_or_else(|_| fail());
        }
    }
    // SAFETY: FinishActor relinquishes this actor permanently and does not return.
    unsafe {
        core::arch::asm!("svc #0", in("x8") SystemCall::FinishActor.number(), options(noreturn));
    }
}

/// Reports an actor-local failure instead of spinning indefinitely after a panic.
#[panic_handler]
fn on_panic(_information: &core::panic::PanicInfo<'_>) -> ! {
    fail()
}

/// Starts the messaging demo after the kernel enters this actor at EL0.
#[unsafe(no_mangle)]
pub extern "C" fn _start() -> ! {
    run()
}
