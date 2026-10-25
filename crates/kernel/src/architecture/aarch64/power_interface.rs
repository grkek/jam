//! The firmware's Power State Coordination Interface (POWER_STATE_COORDINATION_INTERFACE): starting cores.

use foundation::device_tree::PowerStateCoordinationInterfaceConduit;

const POWER_STATE_COORDINATION_INTERFACE_CPU_ON_64: u64 = 0xc400_0003;

unsafe extern "C" {
    /// Where a freshly started core begins, defined in `boot_entry.s`.
    fn secondary_entry();
}

/// What the firmware answered to a request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PowerStateCoordinationInterfaceStatus {
    /// The request succeeded.
    Success,
    /// The firmware does not support the request.
    NotSupported,
    /// An argument was invalid.
    InvalidParameters,
    /// The firmware refused the request.
    Denied,
    /// The core is already running.
    AlreadyOn,
    /// A start request for the core is still in progress.
    OnPending,
    /// The firmware failed internally.
    InternalFailure,
    /// The core does not exist.
    NotPresent,
    /// The core is disabled.
    Disabled,
    /// The entry point address is invalid.
    InvalidAddress,
    /// A value outside the specification.
    Unknown(i64),
}

impl PowerStateCoordinationInterfaceStatus {
    fn from_return_value(return_value: i64) -> Self {
        match return_value {
            0 => Self::Success,
            -1 => Self::NotSupported,
            -2 => Self::InvalidParameters,
            -3 => Self::Denied,
            -4 => Self::AlreadyOn,
            -5 => Self::OnPending,
            -6 => Self::InternalFailure,
            -7 => Self::NotPresent,
            -8 => Self::Disabled,
            -9 => Self::InvalidAddress,
            other => Self::Unknown(other),
        }
    }
}

/// Calls the firmware through the conduit the device tree named and returns `x0`.
fn call_firmware(
    conduit: PowerStateCoordinationInterfaceConduit,
    function_identifier: u64,
    first_argument: u64,
    second_argument: u64,
    third_argument: u64,
) -> i64 {
    let return_value: u64;
    // SAFETY: POWER_STATE_COORDINATION_INTERFACE calls follow the SMC calling convention: arguments in `x0` to `x3`, the result in
    // `x0`, and the other caller-saved registers possibly clobbered, which `clobber_abi` declares.
    // `CPU_ON` starts another core at an address this kernel chose and does not touch the caller's
    // memory.
    unsafe {
        match conduit {
            PowerStateCoordinationInterfaceConduit::HypervisorCall => core::arch::asm!(
                "hvc #0",
                inout("x0") function_identifier => return_value,
                inout("x1") first_argument => _,
                inout("x2") second_argument => _,
                inout("x3") third_argument => _,
                clobber_abi("C"),
                options(nostack),
            ),
            PowerStateCoordinationInterfaceConduit::SecureMonitorCall => core::arch::asm!(
                "smc #0",
                inout("x0") function_identifier => return_value,
                inout("x1") first_argument => _,
                inout("x2") second_argument => _,
                inout("x3") third_argument => _,
                clobber_abi("C"),
                options(nostack),
            ),
        }
    }
    i64::from_ne_bytes(return_value.to_ne_bytes())
}

/// Starts a core at the kernel's secondary entry point.
///
/// `core_identifier` is the core's affinity value, which on QEMU `virt` equals its index.
/// The core receives `translation_table_address` in `x0`, so it can switch on the same
/// memory map as the boot core.
pub fn start_secondary_core(
    conduit: PowerStateCoordinationInterfaceConduit,
    core_identifier: u64,
    translation_table_address: usize,
) -> PowerStateCoordinationInterfaceStatus {
    let entry_point_address = secondary_entry as usize as u64;
    PowerStateCoordinationInterfaceStatus::from_return_value(call_firmware(
        conduit,
        POWER_STATE_COORDINATION_INTERFACE_CPU_ON_64,
        core_identifier,
        entry_point_address,
        translation_table_address as u64,
    ))
}
