//! Scenarios that test scheduling and reclamation rather than messaging.

use core::hint::black_box;

use runtime::inbox;

use crate::{
    bootstrap::Assignment,
    support::{fail, tag, tagged},
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
