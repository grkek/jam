//! Routes a supervisor call to the matching handler.
use core::fmt::Write;

use foundation::{system_call::SystemCall, system_call_status::SystemCallStatus};

use crate::architecture::aarch64::{early_console::EarlyConsole, exceptions::TrapFrame};

use super::actor_calls::{current_address, finish_actor};

const SYSTEM_CALL_NUMBER_REGISTER: usize = 8;
const STATUS_REGISTER: usize = 0;
const VALUE_REGISTER: usize = 1;

/// Handles an SVC from EL0. Writes status (and optional value) into the frame.
///
/// For `FinishActor` the returned pointer may be a different frame (the next actor or idle).
#[must_use]
pub fn dispatch(trap_frame: *mut TrapFrame) -> *mut TrapFrame {
    // SAFETY: The exception vector just saved this frame; we own it until resume.
    let frame = unsafe { &mut *trap_frame };

    let _ = writeln!(EarlyConsole, "SVC x8={:#x}", frame.general_registers[8]);

    let call_number = frame.general_registers[SYSTEM_CALL_NUMBER_REGISTER];
    match SystemCall::try_from(call_number) {
        Ok(SystemCall::CurrentAddress) => {
            let (status, value) = current_address();
            frame.general_registers[STATUS_REGISTER] = status.number();
            frame.general_registers[VALUE_REGISTER] = value;
            trap_frame
        }
        Ok(SystemCall::FinishActor) => finish_actor(trap_frame),
        Ok(_) => {
            frame.general_registers[STATUS_REGISTER] = SystemCallStatus::UnknownSystemCall.number();
            trap_frame
        }
        Err(_) => {
            frame.general_registers[STATUS_REGISTER] = SystemCallStatus::UnknownSystemCall.number();
            trap_frame
        }
    }
}
