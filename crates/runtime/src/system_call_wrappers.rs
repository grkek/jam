//! Register-level system-call entry used by the typed runtime operations.

/// Invokes a call while declaring both result registers as clobbered.
///
/// # Safety
/// Any pointer encoded in `arguments` must remain valid for the operation and
/// have the access permissions and lifetime required by the call's ABI.
#[cfg(target_arch = "aarch64")]
pub(crate) unsafe fn invoke(number: u64, arguments: [u64; 6]) -> (u64, u64) {
    let [
        mut status,
        mut value,
        argument_two,
        argument_three,
        argument_four,
        argument_five,
    ] = arguments;
    // SAFETY: The caller upholds pointer validity. No `nomem` or `readonly`
    // promise is made: the kernel can read or write the caller's buffers.
    unsafe {
        core::arch::asm!("svc #0",
            in("x8") number,
            inout("x0") status,
            inout("x1") value,
            in("x2") argument_two,
            in("x3") argument_three,
            in("x4") argument_four,
            in("x5") argument_five,
            options(nostack));
    }
    (status, value)
}

/// Host builds cannot issue JAM's AArch64 supervisor calls.
///
/// # Safety
/// Same pointer contract as the target implementation; no pointers are used here.
#[cfg(not(target_arch = "aarch64"))]
pub(crate) unsafe fn invoke(_number: u64, _arguments: [u64; 6]) -> (u64, u64) {
    (
        foundation::system_call_status::SystemCallStatus::UnsupportedOperation.number(),
        0,
    )
}
