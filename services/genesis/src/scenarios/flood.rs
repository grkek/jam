//! Many-to-one floods, dead-address spam and inbox-bound probing.

use core::hint::spin_loop;

use foundation::address::LocalAddress;
use runtime::inbox;

use crate::{
    bootstrap::Assignment,
    support::{OrFail, UNBOUNDED_INBOX_LIMIT, require, send_retry, tag, tagged, word},
};

const MAXIMUM_SENDERS: usize = 16;
const DEAD_LETTER_MESSAGES: u64 = 4096;

/// Payload length for message `sequence`: varies so sizes interleave.
fn collector_payload_length(sequence: u64) -> usize {
    16 + (sequence % 48) as usize
}

/// Floods the collector with variable-size, self-describing messages.
pub fn collector_sender(assignment: &Assignment) {
    let mut payload = [0u8; 64];
    for sequence in 0..assignment.param {
        let length = collector_payload_length(sequence);
        payload[..8].copy_from_slice(&assignment.role.to_le_bytes());
        payload[8..16].copy_from_slice(&sequence.to_le_bytes());
        payload[16..length].fill(sequence as u8);
        send_retry(assignment.peer, tag::COLLECT, sequence, &payload[..length]);
    }
}

/// Receives from many senders at once and checks per-sender FIFO order and content.
pub fn collector(assignment: &Assignment, record: &mut [u8]) {
    let senders = usize::try_from(assignment.role).or_fail();
    let per_sender = assignment.param;
    require(senders <= MAXIMUM_SENDERS);
    let mut next = [0u64; MAXIMUM_SENDERS];

    for _ in 0..(senders as u64) * per_sender {
        let message = inbox::await_matching(tagged(tag::COLLECT), record).or_fail();
        let payload = message.payload;

        let index = usize::try_from(word(payload, 0)).or_fail();
        let sequence = word(payload, 1);
        // A mismatch means reordering, duplication or loss within one sender.
        require(index < senders && sequence == next[index]);
        next[index] += 1;

        require(payload.len() == collector_payload_length(sequence));
        require(payload[16..].iter().all(|&byte| byte == sequence as u8));
    }

    require(next[..senders].iter().all(|&count| count == per_sender));
}

/// Spams an address that is dead or dying. Stale addresses must fail cleanly,
/// so every result is ignored; the point is that the kernel survives.
pub fn dead_letter(assignment: &Assignment) {
    for index in 0..DEAD_LETTER_MESSAGES {
        let _ = inbox::tell_actor(assignment.peer, tag::DEAD_LETTER, index, b"into the void");
        if index & 0xff == 0 {
            spin_loop();
        }
    }
}

/// Fills a peer that never receives until the kernel refuses. The bound must exist.
pub fn fill_victim(assignment: &Assignment) {
    let victim: LocalAddress = assignment.peer;
    let mut sent = 0u64;
    while inbox::tell_actor(victim, tag::FILL, sent, &[0xab; 32]).is_ok() {
        sent += 1;
        require(sent < UNBOUNDED_INBOX_LIMIT);
    }
}
