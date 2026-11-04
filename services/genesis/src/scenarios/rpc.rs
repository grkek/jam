//! Server, clients, and a client that dies with a request in flight.

use core::hint::spin_loop;

use runtime::inbox;

use crate::{
    bootstrap::Assignment,
    support::{OrFail, fail, require, selector, send_retry, tag, tagged, word},
};

/// Replies to a dead client can never succeed, so the retry is bounded.
const REPLY_RETRY_LIMIT: u32 = 200_000;

/// Serves `param` requests from any client by echoing the payload.
pub fn server(assignment: &Assignment, record: &mut [u8]) {
    for _ in 0..assignment.param {
        let request = inbox::await_matching(tagged(tag::REQUEST), record).or_fail();
        let client = request.sender;
        let sequence = word(request.payload, 0);

        for _ in 0..REPLY_RETRY_LIMIT {
            if inbox::tell_actor(client, tag::REPLY, sequence, request.payload).is_ok() {
                break;
            }
            spin_loop();
        }
    }
}

/// Builds the request for `sequence` into `payload`; returns its length.
fn build_request(payload: &mut [u8; 48], sequence: u64, index: u64) -> usize {
    let length = 8 + ((sequence * 7 + index) % 40) as usize;
    payload[..8].copy_from_slice(&sequence.to_le_bytes());
    for (position, byte) in payload[8..length].iter_mut().enumerate() {
        *byte = (sequence as u8) ^ (position as u8) ^ (index as u8);
    }
    length
}

/// Request/reply client with a selective await on (server, tag, correlation).
pub fn client(assignment: &Assignment, record: &mut [u8]) {
    let server = assignment.peer;
    let mut payload = [0u8; 48];

    for sequence in 0..assignment.param {
        let length = build_request(&mut payload, sequence, assignment.role);
        send_retry(server, tag::REQUEST, sequence, &payload[..length]);

        let reply = inbox::await_matching(selector(server, tag::REPLY, sequence), record).or_fail();
        require(reply.sender == server && reply.payload == &payload[..length]);
    }
}

/// Sends one request, then dies before the reply arrives.
pub fn client_crash(assignment: &Assignment) -> ! {
    let mut payload = [0u8; 48];
    let length = build_request(&mut payload, 0, assignment.role);
    send_retry(assignment.peer, tag::REQUEST, 0, &payload[..length]);
    fail()
}
