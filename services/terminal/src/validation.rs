//! Optional EL0 integration probes for the observation copy boundary.
//!
//! Compiled only with `observation-tests`; normal terminal startup skips them.

use foundation::{observation, system_call::SystemCall, system_call_status::SystemCallStatus};
/// Test-only entry accepts intentionally invalid addresses for kernel rejection.
///
/// # Safety
/// Calls expected to succeed must provide live buffers with the required access.
unsafe fn invoke(number: u64, arguments: [u64; 6]) -> (u64, u64) {
    let [mut status, mut value, second, third, fourth, fifth] = arguments;
    // SAFETY: The test supplies live storage on successful paths; failing paths
    // deliberately exercise the kernel's checked copy boundary.
    unsafe {
        core::arch::asm!("svc #0", in("x8") number,
            inout("x0") status, inout("x1") value, in("x2") second,
            in("x3") third, in("x4") fourth, in("x5") fifth, options(nostack));
    }
    (status, value)
}

/// Page-aligned private output used to force a cross-page copy.
#[repr(align(4096))]
struct Pages([u8; 8192]);
static mut OUTPUT: Pages = Pages([0; 8192]);

/// Checks one intentionally invalid invocation without dereferencing its pointer.
fn rejects(call: SystemCall, arguments: [u64; 6], expected: SystemCallStatus, value: u64) {
    // SAFETY: Invalid pointers deliberately test kernel validation. No Rust
    // reference is constructed from them, and the kernel must reject the copy.
    let result = unsafe { invoke(call.number(), arguments) };
    if result != (expected.number(), value) {
        super::fail();
    }
}

/// Validates malformed inputs, readonly output, rollback, and a crossing copy.
pub fn run() {
    let capacity = runtime::observation::read_census(0)
        .unwrap_or_else(|_| super::fail())
        .1;
    let mut small = [0x55_u8; 8];
    rejects(
        SystemCall::ReadStatistics,
        [small.as_mut_ptr() as u64, 8, 0, 0, 0, 0],
        SystemCallStatus::BufferTooSmall,
        observation::STATISTICS_RECORD_BYTE_COUNT as u64,
    );
    if small != [0x55; 8] {
        super::fail();
    }
    rejects(
        SystemCall::ReadStatistics,
        [0, 80, 0, 0, 0, 0],
        SystemCallStatus::InvalidUserMemory,
        0,
    );
    rejects(
        SystemCall::ReadStatistics,
        [u64::MAX - 20, 80, 0, 0, 0, 0],
        SystemCallStatus::InvalidUserMemory,
        0,
    );
    rejects(
        SystemCall::ReadStatistics,
        [run as usize as u64, 80, 0, 0, 0, 0],
        SystemCallStatus::InvalidUserMemory,
        0,
    );
    rejects(
        SystemCall::ReadStatistics,
        [0, 80, 1, 0, 0, 0],
        SystemCallStatus::InvalidArgument,
        0,
    );
    rejects(
        SystemCall::ReadCensus,
        [capacity, 0, 64, 0, 0, 0],
        SystemCallStatus::InvalidArgument,
        0,
    );
    rejects(
        SystemCall::ReadCensus,
        [0, 0, 64, 0, 0, 0],
        SystemCallStatus::InvalidUserMemory,
        0,
    );
    rejects(
        SystemCall::ReadCensus,
        [0, 0, 63, 0, 0, 0],
        SystemCallStatus::BufferTooSmall,
        64,
    );
    // SAFETY: This single-threaded actor has an exclusive private mapping of
    // OUTPUT. The range remains inside it and crosses the page at offset 4096.
    let bytes =
        unsafe { core::slice::from_raw_parts_mut((&raw mut OUTPUT.0).cast::<u8>().add(4090), 80) };
    // SAFETY: The valid writable buffer remains exclusively borrowed until return.
    let result = unsafe {
        invoke(
            SystemCall::ReadStatistics.number(),
            [bytes.as_mut_ptr() as u64, 80, 0, 0, 0, 0],
        )
    };
    if result != (SystemCallStatus::Accepted.number(), 80)
        || observation::decode::<10>(bytes).is_none()
    {
        super::fail();
    }
}
