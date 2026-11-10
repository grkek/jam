//! Two-actor scenarios: checked copies, then selective request/reply.

use foundation::{
    address::LocalAddress, stress::PING_PONG_EXCHANGES, system_call_status::SystemCallStatus,
    tag::Tag,
};
use runtime::inbox;

use crate::{
    bootstrap::Assignment,
    support::{
        MESSAGE_HEADER_BYTES, OrFail, PAGE_SIZE, Scratch, check_scalar_contracts, current_address,
        expect_status, receive_into_raw_buffer, require, selector, send_retry, tag, take_scratch,
    },
};

const COPY_PAYLOAD_BYTES: usize = 32;
const COPY_OUTPUT_BYTES: usize = 64;
/// Both buffers start six bytes before the page boundary so the copy straddles it.
const COPY_OFFSET: usize = PAGE_SIZE - 6;
const COPY_PATTERN: u8 = 0x5a;
const COPY_CORRELATION: u64 = 99;

/// Near-miss mail parked in the inbox for the whole exchange. Each entry shares a
/// tag or correlation with a live selector but comes from the wrong sender (self).
const NOISE: [(Tag, u64, &[u8]); 4] = [
    (tag::PING, 0, b"near-miss-first"),
    (tag::PING, 5, b"near-miss-mid"),
    (tag::NOISE_OTHER, 0, b"wrong-tag"),
    // Deliberately never received: drained by the kernel at exit.
    (tag::LEFTOVER, 0, b"left queued for exit cleanup"),
];

/// Checked copies across a page boundary, an unconsumed too-small receive, an
/// invalid output pointer, and the scalar SystemCall contracts.
fn copy_validation(own: LocalAddress) {
    let Scratch { input, output } = take_scratch();
    let input = &mut input[COPY_OFFSET..COPY_OFFSET + COPY_PAYLOAD_BYTES];
    let output = &mut output[COPY_OFFSET..COPY_OFFSET + COPY_OUTPUT_BYTES];
    let required = COPY_PAYLOAD_BYTES + MESSAGE_HEADER_BYTES;

    input.fill(COPY_PATTERN);
    send_retry(own, tag::COPY_CHECK, COPY_CORRELATION, input);
    let wanted = selector(own, tag::COPY_CHECK, COPY_CORRELATION);

    let small = inbox::receive_message(wanted, &mut []).err().or_fail();
    require(
        small.status == SystemCallStatus::BufferTooSmall
            && small.required_byte_count as usize == required,
    );

    // The invalid pointer must be rejected without consuming the message.
    expect_status(
        receive_into_raw_buffer(u64::MAX - 20, required as u64),
        SystemCallStatus::InvalidUserMemory,
    );

    let message = inbox::receive_message(wanted, output).or_fail();
    require(message.payload == input && message.sender == own);

    check_scalar_contracts(1);
}

/// Copy validation, then 128 selective exchanges with noise in the queue.
/// `param != 0` makes both actors die at that sequence.
pub fn ping_pong(assignment: &Assignment, record: &mut [u8]) {
    let own = current_address();
    let Assignment {
        peer,
        role,
        param: die_after,
        ..
    } = *assignment;
    let initiator = role == 0;

    copy_validation(own);

    for (tag, correlation, body) in NOISE {
        send_retry(own, tag, correlation, body);
    }

    for sequence in 0..PING_PONG_EXCHANGES {
        let dies_here = die_after != 0 && sequence == die_after;

        if initiator {
            send_retry(peer, tag::PING, sequence, b"ping");
            if dies_here {
                crate::support::fail(); // dies with a request in flight
            }
        }

        let message = inbox::await_matching(selector(peer, tag::PING, sequence), record).or_fail();
        let expected: &[u8] = if initiator { b"pong" } else { b"ping" };
        require(message.sender == peer && message.payload == expected);

        if !initiator {
            if dies_here {
                crate::support::fail(); // dies without replying
            }
            send_retry(peer, tag::PING, sequence, b"pong");
        }
    }

    // The noise must have survived every exchange. Receive it out of order,
    // last first; the tag-99 entry is left for exit cleanup.
    for (tag, correlation, body) in [NOISE[2], NOISE[1], NOISE[0]] {
        let message = inbox::receive_message(selector(own, tag, correlation), record).or_fail();
        require(message.sender == own && message.payload == body);
    }
}
