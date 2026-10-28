//! The firmware's Power State Coordination Interface (`PSCI`): starting cores.

use core::sync::atomic::{AtomicU8, Ordering};

use foundation::device_tree::PowerStateCoordinationInterfaceConduit;

const POWER_STATE_COORDINATION_INTERFACE_CPU_ON_64: u64 = 0xc400_0003;
const POWER_STATE_COORDINATION_INTERFACE_SYSTEM_OFF: u64 = 0x8400_0008;

const CONDUIT_UNKNOWN: u8 = 0;
const CONDUIT_HYPERVISOR_CALL: u8 = 1;
const CONDUIT_SECURE_MONITOR_CALL: u8 = 2;
static KNOWN_CONDUIT: AtomicU8 = AtomicU8::new(CONDUIT_UNKNOWN);

unsafe extern "C" {
    /// The physical address of `secondary_entry`, stored as data by `boot_entry.s`.
    static secondary_entry_physical_address: u64;
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
    // SAFETY: PSCI calls follow the SMC calling convention: arguments in `x0` to `x3`, the result in
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

/// Asks the firmware to power the machine off.
pub fn system_off(conduit: PowerStateCoordinationInterfaceConduit) -> ! {
    let _ = super::processor::mask_interrupts();
    let _ = call_firmware(
        conduit,
        POWER_STATE_COORDINATION_INTERFACE_SYSTEM_OFF,
        0,
        0,
        0,
    );
    // The firmware did not power us off. Stay stopped.
    loop {
        super::processor::wait_for_event();
    }
}

/// Remembers how to reach the firmware, so code with no device tree in hand, such as the fault
/// handler, can still power the machine off.
pub fn record_conduit(conduit: Option<PowerStateCoordinationInterfaceConduit>) {
    let encoded = match conduit {
        Some(PowerStateCoordinationInterfaceConduit::HypervisorCall) => CONDUIT_HYPERVISOR_CALL,
        Some(PowerStateCoordinationInterfaceConduit::SecureMonitorCall) => {
            CONDUIT_SECURE_MONITOR_CALL
        }
        None => CONDUIT_UNKNOWN,
    };
    KNOWN_CONDUIT.store(encoded, Ordering::Release);
}

/// Powers the machine off through the recorded conduit, or parks the core when none is known.
pub fn halt_machine() -> ! {
    match KNOWN_CONDUIT.load(Ordering::Acquire) {
        CONDUIT_HYPERVISOR_CALL => {
            system_off(PowerStateCoordinationInterfaceConduit::HypervisorCall)
        }
        CONDUIT_SECURE_MONITOR_CALL => {
            system_off(PowerStateCoordinationInterfaceConduit::SecureMonitorCall)
        }
        _ => {
            let _ = super::processor::mask_interrupts();
            loop {
                super::processor::wait_for_event();
            }
        }
    }
}

/// Starts a core at the kernel's secondary entry point.
///
/// `core_identifier` is the core's affinity value, which on QEMU `virt` equals its index.
/// The core receives `translation_table_address` in `x0`, so it can switch on the same
/// memory map as the boot core.
#[must_use]
pub fn start_secondary_core(
    conduit: PowerStateCoordinationInterfaceConduit,
    core_identifier: u64,
    translation_table_address: usize,
) -> PowerStateCoordinationInterfaceStatus {
    // SAFETY: A plain 64-bit constant defined by the assembly and never written.
    let entry_point_address = unsafe { secondary_entry_physical_address };
    PowerStateCoordinationInterfaceStatus::from_return_value(call_firmware(
        conduit,
        POWER_STATE_COORDINATION_INTERFACE_CPU_ON_64,
        core_identifier,
        entry_point_address,
        translation_table_address as u64,
    ))
}
