//! Routes a supervisor call to the matching handler.

use foundation::{
    system_call::{STATUS_REGISTER, SYSTEM_CALL_NUMBER_REGISTER, SystemCall, VALUE_REGISTER},
    system_call_status::SystemCallStatus,
};

use crate::architecture::aarch64::exceptions::TrapFrame;

use super::actor_calls::{current_address, finish_actor};

/// Handles an SVC from EL0 and writes its result into the saved frame.
///
/// `FinishActor` may return another actor's frame or the idle frame.
#[must_use]
pub fn dispatch(trap_frame: *mut TrapFrame) -> *mut TrapFrame {
    // SAFETY: The exception vector supplied a valid, exclusively owned frame.
    let frame = unsafe { &mut *trap_frame };
    let call_number = frame.general_registers[usize::from(SYSTEM_CALL_NUMBER_REGISTER)];

    match SystemCall::try_from(call_number) {
        Ok(SystemCall::SpawnActor) => unsupported_call(trap_frame),
        Ok(SystemCall::TerminateActor) => unsupported_call(trap_frame),
        Ok(SystemCall::WatchActor) => unsupported_call(trap_frame),
        Ok(SystemCall::UnwatchActor) => unsupported_call(trap_frame),
        Ok(SystemCall::BecomeGuardian) => unsupported_call(trap_frame),

        Ok(SystemCall::TellActor) => super::messaging_calls::tell_actor(trap_frame),

        Ok(SystemCall::AwaitInbox) => super::messaging_calls::await_inbox(trap_frame),

        Ok(SystemCall::FinishActor) => finish_actor(trap_frame),
        Ok(SystemCall::CurrentAddress) => {
            let (status, value) = current_address();

            frame.general_registers[usize::from(STATUS_REGISTER)] = status.number();
            frame.general_registers[usize::from(VALUE_REGISTER)] = value;

            trap_frame
        }
        Ok(SystemCall::AdjustConfiguration) => unsupported_call(trap_frame),
        Ok(SystemCall::AllocatePages) => {
            super::memory_calls::allocate_pages(frame);
            trap_frame
        }
        Ok(SystemCall::ReleasePages) => {
            super::memory_calls::release_pages(frame);
            trap_frame
        }
        Ok(SystemCall::ReadCensus) => {
            super::observation_calls::read(frame, false);
            trap_frame
        }
        Ok(SystemCall::ReadStatistics) => {
            super::observation_calls::read(frame, true);
            trap_frame
        }
        Ok(SystemCall::ReadCoreCensus) => unsupported_call(trap_frame),
        Ok(SystemCall::DrainChronicle) => unsupported_call(trap_frame),
        Ok(SystemCall::ConfigureChronicle) => unsupported_call(trap_frame),
        Ok(SystemCall::CreateScroll) => unsupported_call(trap_frame),
        Ok(SystemCall::SealScroll) => unsupported_call(trap_frame),
        Ok(SystemCall::MapScroll) => unsupported_call(trap_frame),
        Ok(SystemCall::UnmapScroll) => unsupported_call(trap_frame),
        Ok(SystemCall::ReleaseScroll) => unsupported_call(trap_frame),

        Ok(SystemCall::ReceiveMessage) => super::messaging_calls::receive_message(trap_frame),

        Ok(SystemCall::YieldExecution) => unsupported_call(trap_frame),
        Ok(SystemCall::ReadMonotonicTime) => unsupported_call(trap_frame),
        Ok(SystemCall::MapDeviceMemory) => unsupported_call(trap_frame),
        Ok(SystemCall::UnmapDeviceMemory) => unsupported_call(trap_frame),
        Ok(SystemCall::BindInterrupt) => unsupported_call(trap_frame),
        Ok(SystemCall::UnbindInterrupt) => unsupported_call(trap_frame),
        Ok(SystemCall::CompleteInterrupt) => unsupported_call(trap_frame),
        Ok(SystemCall::ReadAutopsy) => unsupported_call(trap_frame),
        Ok(SystemCall::ReadBlackbox) => unsupported_call(trap_frame),
        Ok(SystemCall::DirectMemoryAccessAllocate) => unsupported_call(trap_frame),
        Ok(SystemCall::DirectMemoryAccessRelease) => unsupported_call(trap_frame),
        Ok(SystemCall::DirectMemoryAccessSynchronize) => unsupported_call(trap_frame),
        Err(_) => unsupported_call(trap_frame),
    }
}

/// Rejects an unknown or unimplemented call without terminating the actor.
fn unsupported_call(trap_frame: *mut TrapFrame) -> *mut TrapFrame {
    // SAFETY: Dispatch supplies a valid, exclusively owned saved frame.
    let frame = unsafe { &mut *trap_frame };

    frame.general_registers[usize::from(STATUS_REGISTER)] =
        SystemCallStatus::UnknownSystemCall.number();
    frame.general_registers[usize::from(VALUE_REGISTER)] = 0;

    trap_frame
}
