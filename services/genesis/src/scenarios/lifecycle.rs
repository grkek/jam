//! Scenarios that test scheduling and reclamation rather than messaging.

use core::hint::black_box;

use foundation::{
    stress::{RACE_START_TAG, RACE_WAKE_TAG},
    system_call_status::SystemCallStatus,
};
use runtime::inbox;

use crate::{
    bootstrap::Assignment,
    support::{OrFail, fail, require, tag, tagged},
};

const DEFAULT_SPIN_ITERATIONS: u64 = 100_000_000;

/// Burns CPU. Preemption must let every other actor keep making progress.
pub fn spin_hog(assignment: &Assignment) {
    let iterations = match assignment.param {
        0 => DEFAULT_SPIN_ITERATIONS,
        explicit => explicit,
    };
    let mut accumulator = 0u64;
    for index in 0..iterations {
        accumulator = black_box(accumulator.wrapping_add(index));
    }
    black_box(accumulator);
}

/// Waits for a message nobody sends, so the actor is reclaimed with its inbox full.
pub fn block_forever(record: &mut [u8]) -> ! {
    let _ = inbox::await_matching(tagged(tag::NEVER_SENT), record);
    fail()
}

/// Exits normally after receiving a race sender's matching wake message.
pub fn race_receiver(record: &mut [u8]) {
    let message = inbox::await_matching(tagged(RACE_WAKE_TAG), record).or_fail();
    require(message.payload == b"wake");
}

/// After a kernel start signal, races to send a wakeup to the assigned peer.
///
/// A stale destination is expected when the kernel's termination wins.
pub fn race_sender(assignment: &Assignment, record: &mut [u8]) {
    let _ = inbox::await_matching(tagged(RACE_START_TAG), record).or_fail();
    let result = inbox::tell_actor(assignment.peer, RACE_WAKE_TAG, 0, b"wake");
    require(matches!(
        result,
        Ok(())
            | Err(inbox::MessagingError {
                status: SystemCallStatus::MalformedAddress,
                ..
            })
    ));
}
