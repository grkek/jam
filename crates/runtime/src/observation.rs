//! Safe wrappers for pointer-free actor census and node statistics records.

use crate::system_call_wrappers::invoke;
use foundation::{
    observation::{self, ActorRecord, StatisticsRecord},
    system_call::SystemCall,
    system_call_status::SystemCallStatus,
};

/// Converts a kernel status without assuming that unknown values mean success.
fn accepted(status: u64) -> Result<(), SystemCallStatus> {
    let status = SystemCallStatus::try_from(status).unwrap_or(SystemCallStatus::UnknownSystemCall);
    if status == SystemCallStatus::Accepted {
        Ok(())
    } else {
        Err(status)
    }
}

/// Reads one slot and returns its record together with the actor table capacity.
///
/// # Errors
/// Returns a kernel rejection or `InvalidArgument` for an unsupported record version.
pub fn read_census(slot: u64) -> Result<(ActorRecord, u64), SystemCallStatus> {
    let mut bytes = [0_u8; observation::ACTOR_RECORD_BYTE_COUNT];
    // SAFETY: The output buffer stays exclusively borrowed until the copy finishes.
    let (status, capacity) = unsafe {
        invoke(
            SystemCall::ReadCensus.number(),
            [slot, bytes.as_mut_ptr() as u64, bytes.len() as u64, 0, 0, 0],
        )
    };
    accepted(status)?;
    let record = observation::decode(&bytes).ok_or(SystemCallStatus::InvalidArgument)?;
    Ok((record, capacity))
}

/// Reads diagnostic counters sampled independently across kernel subsystems.
///
/// # Errors
/// Returns a kernel rejection or `InvalidArgument` for a malformed record.
pub fn read_statistics() -> Result<StatisticsRecord, SystemCallStatus> {
    let mut bytes = [0_u8; observation::STATISTICS_RECORD_BYTE_COUNT];
    // SAFETY: The output buffer stays exclusively borrowed until the copy finishes.
    let (status, length) = unsafe {
        invoke(
            SystemCall::ReadStatistics.number(),
            [bytes.as_mut_ptr() as u64, bytes.len() as u64, 0, 0, 0, 0],
        )
    };
    accepted(status)?;
    if length != bytes.len() as u64 {
        return Err(SystemCallStatus::InvalidArgument);
    }
    observation::decode(&bytes).ok_or(SystemCallStatus::InvalidArgument)
}
