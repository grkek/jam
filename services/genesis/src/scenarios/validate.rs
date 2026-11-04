//! Self-checking scenarios: inbox capacity and leaks, payload sizes, hostile SystemCalls.

use core::hint::spin_loop;

use foundation::{address::LocalAddress, system_call_status::SystemCallStatus};
use runtime::inbox;

use crate::{
    bootstrap::Assignment,
    support::{
        check_scalar_contracts, current_address, expect_status, fail_at, output_address, receive_into_raw_buffer, require, selector, send_retry, tag, tagged, take_scratch, OrFail, Rng, Scratch, MESSAGE_HEADER_BYTES, PAGE_SIZE, UNBOUNDED_INBOX_LIMIT
    },
};

const SELF_CYCLES: u64 = 24;
const OVERFLOW_PROBES: usize = 4;
/// Strict mode runs on a quiet system, so prolonged starvation is a real failure.
const STARVATION_LIMIT: u32 = 10_000;

#[derive(Clone, Copy)]
enum DrainOrder {
    Forward,
    Reverse,
    Interleaved,
}

impl DrainOrder {
    fn for_cycle(cycle: u64) -> Self {
        match cycle % 3 {
            0 => Self::Forward,
            1 => Self::Reverse,
            _ => Self::Interleaved,
        }
    }
}

/// Fills the actor's own inbox until the kernel refuses; returns the count.
fn fill_own_inbox(own: LocalAddress) -> u64 {
    let mut count = 0;
    while inbox::tell_actor(own, tag::SELF_FILL, count, &count.to_le_bytes()).is_ok() {
        count += 1;
        require(count < UNBOUNDED_INBOX_LIMIT);
    }
    count
}

fn take_one(own: LocalAddress, index: u64, buffer: &mut [u8]) {
    let message = inbox::receive_message(selector(own, tag::SELF_FILL, index), buffer).or_fail();
    require(message.sender == own && message.payload == &index.to_le_bytes()[..]);
}

/// Drains `count` self-sent messages in the given order and checks nothing is left.
fn drain_self(own: LocalAddress, count: u64, order: DrainOrder) {
    let mut buffer = [0u8; 64];
    match order {
        DrainOrder::Forward => (0..count).for_each(|index| take_one(own, index, &mut buffer)),
        DrainOrder::Reverse => (0..count)
            .rev()
            .for_each(|index| take_one(own, index, &mut buffer)),
        DrainOrder::Interleaved => {
            (0..count)
                .step_by(2)
                .for_each(|index| take_one(own, index, &mut buffer));
            (0..count)
                .filter(|index| index % 2 == 1)
                .rev()
                .for_each(|index| take_one(own, index, &mut buffer));
        }
    }
    require(inbox::receive_message(tagged(tag::SELF_FILL), &mut buffer).is_err());
}

/// Fills and drains the own inbox repeatedly. The inbox is bounded by a pool
/// shared with other actors, so the capacity only has to be stable when the system
/// is quiet: `param != 0` demands identical capacity every cycle and refused overflow.
pub fn self_cycle(assignment: &Assignment) {
    let own = current_address();
    let strict = assignment.param != 0;
    let mut baseline = None;
    let mut starved = 0u32;
    let mut cycle = 0;

    while cycle < SELF_CYCLES {
        let count = fill_own_inbox(own);

        if count == 0 {
            // The shared pool is exhausted by other actors; wait for it.
            starved += 1;
            require(!strict || starved <= STARVATION_LIMIT);
            spin_loop();
            continue;
        }

        if strict {
            require(*baseline.get_or_insert(count) == count);
            for _ in 0..OVERFLOW_PROBES {
                require(inbox::tell_actor(own, tag::SELF_FILL, count, b"overflow").is_err());
            }
        }

        drain_self(own, count, DrainOrder::for_cycle(cycle));
        cycle += 1;
    }
}

const PAYLOAD_SIZES: &[usize] = &[
    0, 1, 2, 3, 7, 8, 15, 16, 31, 32, 63, 64, 65, 127, 128, 129, 255, 256, 257, 511, 512, 513,
    1023, 1024, 1025, 2047, 2048, 2049, 4095, 4096, 4097, 8000, 8168,
];
/// Sizes up to here must always be accepted (after retrying through pool pressure).
const GUARANTEED_PAYLOAD_BYTES: usize = 1024;

/// Offset within a two-page buffer that makes `length` bytes straddle the middle
/// page boundary (or end flush with the buffer when too long to straddle).
fn straddle(length: usize) -> usize {
    if length <= PAGE_SIZE {
        PAGE_SIZE - length / 2
    } else {
        2 * PAGE_SIZE - length
    }
}

fn fill_pattern(payload: &mut [u8], seed: usize) {
    for (index, byte) in payload.iter_mut().enumerate() {
        *byte = (index as u8).wrapping_mul(31).wrapping_add(seed as u8);
    }
}

/// Sends payloads of many sizes from page-crossing offsets to itself. A short buffer
/// must report the exact size without consuming. Large payloads may legitimately be
/// refused, so only the guaranteed sizes are required to be accepted.
pub fn size_probe() {
    let own = current_address();
    let Scratch { input, output } = take_scratch();

    for &size in PAYLOAD_SIZES {
        let offset = straddle(size);
        fill_pattern(&mut input[offset..offset + size], size);
        let payload = &input[offset..offset + size];
        let correlation = size as u64;

        if size <= GUARANTEED_PAYLOAD_BYTES {
            send_retry(own, tag::SIZE_PROBE, correlation, payload);
        } else if inbox::tell_actor(own, tag::SIZE_PROBE, correlation, payload).is_err() {
            continue;
        }

        let wanted = selector(own, tag::SIZE_PROBE, correlation);
        let required = size + MESSAGE_HEADER_BYTES;
        let out = straddle(required);

        if size > 0 {
            let short = inbox::receive_message(wanted, &mut output[out..out + required - 1])
                .err()
                .or_fail();
            require(
                short.status == SystemCallStatus::BufferTooSmall
                    && short.required_byte_count as usize == required,
            );
        }

        let message = inbox::receive_message(wanted, &mut output[out..out + required]).or_fail();
        require(message.sender == own && message.payload == payload);
    }

    require(inbox::receive_message(tagged(tag::SIZE_PROBE), &mut output[..64]).is_err());
}

const FUZZ_ROUNDS: u32 = 2048;
const SENTINEL: [u8; 8] = [7; 8];
const SENTINEL_CORRELATION: u64 = 99;

/// A hostile (pointer, length) pair for `ReceiveMessage`.
fn hostile_buffer(rng: &mut Rng, valid_base: u64) -> (u64, u64, u64) {
    match rng.next_u64() % 3 {
        // Pointer plus length wraps the address space.
        0 => (u64::MAX - rng.next_u64() % 64, 32 + rng.next_u64() % 64, 0),
        // Kernel half of the address space.
        1 => (0xffff_0000_0000_0000 | (rng.next_u64() & 0x0000_ffff_ffff_fff8), 32 + rng.next_u64() % 64, 1,
        ),
        // Valid base, absurd length.
        _ => (valid_base, u64::MAX - rng.next_u64() % 4096, u64::MAX),
    }
}

/// Throws hostile SystemCall arguments at the kernel. Invalid user memory must be
/// rejected without consuming the matching message, and unsupported timeouts refused.
pub fn system_call_fuzz(assignment: &Assignment) {
    let own = current_address();
    let mut rng = Rng::new(assignment.seed);
    let valid_base = output_address() as u64;

    send_retry(own, tag::COPY_CHECK, SENTINEL_CORRELATION, &SENTINEL);

    for _ in 0..FUZZ_ROUNDS {
        let (kind, pointer, length) = hostile_buffer(&mut rng, valid_base);
        if receive_into_raw_buffer(pointer, length) != SystemCallStatus::InvalidUserMemory.number() {
            match kind {
                0 => fail_at::<0x101>(), // pointer + length wraps
                1 => fail_at::<0x102>(), // kernel-half pointer
                _ => fail_at::<0x103>(), // valid base, absurd length
            }
        }
        expect_status(
            receive_into_raw_buffer(pointer, length),
            SystemCallStatus::InvalidUserMemory,
        );
        check_scalar_contracts(rng.next_u64() | 1);
    }

    // None of the rejected calls may have consumed the sentinel.
    let mut buffer = [0u8; 64];
    let message = inbox::receive_message(
        selector(own, tag::COPY_CHECK, SENTINEL_CORRELATION),
        &mut buffer,
    )
    .or_fail();
    require(message.payload == SENTINEL);
}
