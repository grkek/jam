//! EL0 failure-recovery probe: live delivery first, stale rejection after exit.

use foundation::{
    address::LocalAddress,
    stress::{
        LOAD_START_TAG, RECOVERY_VERIFY_TAG, REINCARNATION_STALE_SENDS_PER_MESSAGE,
        REINCARNATION_STOP_TAG,
    },
    system_call_status::SystemCallStatus,
};
use runtime::inbox;

use crate::{
    bootstrap::Assignment,
    support::{OrFail, require, send_retry, tag, tagged},
};

/// Maximum stale-address sends required from a single recovery probe.
const STALE_SEND_COUNT: u64 = 32;

/// Validates delivery to a live actor, followed by rejection of its stale address.
///
/// The kernel holds the victim in `Waiting` on an unrelated selector, so the
/// first send must enter its inbox without waking it. The probe then waits for
/// the kernel's confirmation that the victim was fully reclaimed; the second
/// phase verifies that no stale send can reach a reused slot generation.
pub fn probe(assignment: &Assignment, record: &mut [u8]) {
    let start = inbox::await_matching(tagged(LOAD_START_TAG), record).or_fail();
    require(start.payload.is_empty());

    inbox::tell_actor(assignment.peer, tag::DEAD_LETTER, 0, b"live victim").or_fail();

    let confirmation = inbox::await_matching(tagged(RECOVERY_VERIFY_TAG), record).or_fail();
    require(confirmation.payload.is_empty());
    for sequence in 0..STALE_SEND_COUNT {
        let rejection = inbox::tell_actor(
            assignment.peer,
            tag::DEAD_LETTER,
            sequence + 1,
            b"stale victim",
        );
        require(matches!(
            rejection,
            Err(error) if error.status == SystemCallStatus::MalformedAddress
        ));
    }
}

/// Exercises an old incarnation while a new collector occupies the same slot.
///
/// The old destination is stored in the bootstrap seed; the fresh address is
/// the peer. The collector independently checks role, sequence, and payload.
/// A stale admission is always a test failure, even if the collector completes.
pub fn reincarnation_probe(assignment: &Assignment, record: &mut [u8]) {
    let old_address = LocalAddress::from_packed(assignment.seed);
    require(old_address.slot_index() == assignment.peer.slot_index());
    require(old_address.generation_counter() != assignment.peer.generation_counter());

    let start = inbox::await_matching(tagged(LOAD_START_TAG), record).or_fail();
    require(start.payload.is_empty());

    let mut payload = [0u8; 64];
    for sequence in 0..assignment.param {
        for stale_index in 0..REINCARNATION_STALE_SENDS_PER_MESSAGE {
            let rejection = inbox::tell_actor(
                old_address,
                tag::COLLECT,
                sequence * REINCARNATION_STALE_SENDS_PER_MESSAGE + stale_index,
                b"wrong incarnation",
            );
            require(matches!(
                rejection,
                Err(error) if error.status == SystemCallStatus::MalformedAddress
            ));
        }

        let byte_count = 16 + usize::try_from(sequence % 48).or_fail();
        payload[..8].copy_from_slice(&assignment.role.to_le_bytes());
        payload[8..16].copy_from_slice(&sequence.to_le_bytes());
        payload[16..byte_count].fill(sequence as u8);
        send_retry(
            assignment.peer,
            tag::COLLECT,
            sequence,
            &payload[..byte_count],
        );
    }
}

/// Sends normal collector traffic but stays resident until the test ends.
///
/// Keeping senders alive prevents their slots from being recycled before the
/// test completes all generations of the separate target slot.
pub fn reincarnation_traffic_sender(assignment: &Assignment, record: &mut [u8]) {
    let start = inbox::await_matching(tagged(LOAD_START_TAG), record).or_fail();
    require(start.payload.is_empty());
    super::flood::collector_sender(assignment);

    let stop = inbox::await_matching(tagged(REINCARNATION_STOP_TAG), record).or_fail();
    require(stop.payload.is_empty());
}
