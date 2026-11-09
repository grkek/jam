//! Normal boot's temporary EL1 console driver and independent EL0 terminal.
//!
//! Input is polled on scheduled turns and returned only after a terminal read
//! request. Output uses acknowledgements; failed admissions retain a pending
//! response and never repeat physical output. Only the boot-selected terminal
//! can use the driver. Genesis remains a separate EL0 actor.
//! This is bootstrap plumbing, not the future Steward supervision hierarchy.

use crate::{
    actors::{
        inbox::MessageMetadata,
        table::{spawn_kernel_actor, spawn_user_actor, spawn_user_actor_with_argument},
    },
    architecture::aarch64::{early_console::EarlyConsole, processor},
    memory::{address_translation::physical_to_virtual, user_program::EL0_GENESIS_PROGRAM},
    messaging::{delivery::tell_actor, kernel_inbox},
    system_calls::actor_calls::{current_address, pack_local_address},
};
use core::sync::atomic::{AtomicU64, Ordering};
use foundation::{
    address::LocalAddress, configuration::Configuration, console,
    system_call_status::SystemCallStatus,
};

static TERMINAL_ADDRESS: AtomicU64 = AtomicU64::new(0);
/// Terminal ELF built independently from Genesis by xtask.
static TERMINAL_PROGRAM: &[u8] = include_bytes!(env!("TERMINAL_ELF_PATH"));

/// Decodes a boot-authenticated local identity.
fn address(packed: u64) -> LocalAddress {
    let bytes = packed.to_le_bytes();
    let (slot, generation) = bytes.split_at(4);
    LocalAddress::new(
        u32::from_le_bytes(slot.try_into().unwrap_or([0; 4])),
        u32::from_le_bytes(generation.try_into().unwrap_or([0; 4])),
    )
}

/// Starts normal userland and returns an error if a required actor cannot start.
///
/// # Errors
/// Returns an actor-creation failure description.
pub fn start() -> Result<(), &'static str> {
    let genesis = spawn_user_actor(EL0_GENESIS_PROGRAM, None, Configuration::default())
        .map_err(|_| "Genesis creation failed")?;
    let driver = spawn_kernel_actor(run_driver, None, Configuration::default())
        .map_err(|_| "console driver creation failed")?;
    let terminal = spawn_user_actor_with_argument(
        TERMINAL_PROGRAM,
        Some(genesis),
        Configuration::default(),
        pack_local_address(driver),
    )
    .map_err(|_| "terminal creation failed")?;
    crate::information!(
        EarlyConsole,
        "Userland started: Genesis {genesis}, Terminal {terminal}, Console-Driver {driver}"
    );
    TERMINAL_ADDRESS.store(pack_local_address(terminal), Ordering::Release);
    Ok(())
}

/// A retained response prevents output duplication or input loss on backpressure.
struct Response {
    tag: u64,
    correlation: u64,
    length: usize,
    bytes: [u8; console::MAXIMUM_INPUT_BYTE_COUNT],
}

/// Polls UART receive and authenticated output requests in bounded batches.
extern "C" fn run_driver() -> ! {
    let (_, own) = current_address();
    let own = address(own);
    let terminal = loop {
        let packed = TERMINAL_ADDRESS.load(Ordering::Acquire);
        if packed != 0 {
            break address(packed);
        }
        processor::wait_for_event();
    };
    let base = physical_to_virtual(crate::platform::current::PL011_UART_PHYSICAL_BASE_ADDRESS);
    let mut requested_read = None;
    let mut pending: Option<Response> = None;
    let mut output = [0_u8; console::MAXIMUM_OUTPUT_BYTE_COUNT];
    loop {
        if let Some(response) = pending.as_ref() {
            let bytes = response.bytes.get(..response.length).unwrap_or(&[]);
            let result = tell_actor(
                terminal,
                MessageMetadata {
                    sender: own,
                    tag: response.tag,
                    correlation: response.correlation,
                },
                bytes,
            );
            match result {
                Ok(()) => pending = None,
                Err(SystemCallStatus::BudgetExhausted) => {}
                Err(_) => {
                    crate::error!(EarlyConsole, "Console response failed");
                    loop {
                        processor::wait_for_event();
                    }
                }
            }
        }
        if pending.is_none() {
            if let Some((metadata, length)) = kernel_inbox::receive(&mut output) {
                if metadata.sender == terminal {
                    if metadata.tag == console::WRITE_TAG {
                        EarlyConsole::write_bytes(output.get(..length).unwrap_or(&[]));
                        pending = Some(Response {
                            tag: console::WRITTEN_TAG,
                            correlation: metadata.correlation,
                            length: 0,
                            bytes: [0; console::MAXIMUM_INPUT_BYTE_COUNT],
                        });
                    } else if metadata.tag == console::READ_TAG
                        && length == 0
                        && requested_read.is_none()
                    {
                        requested_read = Some(metadata.correlation);
                    }
                }
            }
        }
        if pending.is_none() {
            if let Some(correlation) = requested_read {
                let mut response = Response {
                    tag: console::INPUT_TAG,
                    correlation,
                    length: 0,
                    bytes: [0; console::MAXIMUM_INPUT_BYTE_COUNT],
                };
                for byte in &mut response.bytes {
                    // SAFETY: Boot maps this platform's PL011 as Device memory.
                    // This actor is the sole receive owner; only it calls read_byte.
                    let Some(received) = (unsafe { pl011_console::read_byte(base) }) else {
                        break;
                    };
                    *byte = received;
                    response.length += 1;
                }
                if response.length != 0 {
                    requested_read = None;
                    pending = Some(response);
                }
            }
        }
        if pending.is_none() {
            processor::wait_for_event();
        }
        // Pending sends retry on a later scheduling turn if the pool is full.
        else {
            core::hint::spin_loop();
        }
    }
}
