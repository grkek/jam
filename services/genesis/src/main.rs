//! Multi-actor EL0 messaging stress program.
//!
//! The kernel boots many instances of this one binary and sends each a tag-zero
//! bootstrap message of five little-endian u64 words:
//!   [packed peer address, role, scenario, seed, param]
//! The scenario selects what the actor does. Healthy scenarios verify the
//! messaging invariants and then finish; fault scenarios die on purpose in different
//! ways so the kernel's isolation, cleanup and dead-address handling get exercised
//! while the healthy actors keep hammering the message pool.
//!
//! Scenario IDs must stay in sync with the kernel's `stress_scenario` module.

#![no_std]
#![no_main]

use core::hint::{black_box, spin_loop};

use foundation::{
    address::LocalAddress, system_call::SystemCall, system_call_status::SystemCallStatus,
};
use runtime::inbox::{self, MessageSelector};

/// Message tag width used by the runtime. Adjust if the runtime uses another type.
type Tag = u64;

// Healthy scenarios.
const PING_PONG: u64 = 0; // copy validation + 128 selective exchanges (param = die-after sequence)
const COLLECTOR_SENDER: u64 = 1; // floods a collector (role = index, param = count)
const COLLECTOR: u64 = 2; // verifies per-sender FIFO order (role = senders, param = count each)
const SELF_CYCLE: u64 = 3; // fill own inbox, drain in several orders, leak check
const SIZE_PROBE: u64 = 4; // payload sizes and page-crossing offsets
const SYSCALL_FUZZ: u64 = 5; // invalid pointers / unsupported options
const SERVER: u64 = 6; // replies to requests (param = total requests)
const CLIENT: u64 = 7; // request/reply (role = index, param = count)
const CLIENT_CRASH: u64 = 8; // sends one request, then dies before the reply
const QUICK_EXIT: u64 = 9; // spawn/exit churn
const SPIN_HOG: u64 = 10; // burns CPU to prove preemption
const DEAD_LETTER: u64 = 11; // spams an address that is (about to be) dead
const FILL_VICTIM: u64 = 12; // fills a blocked peer's inbox until the kernel refuses
const BLOCK_FOREVER: u64 = 13; // orphan that never receives what it waits for

// Fault scenarios.
const CRASH_BRK: u64 = 20;
const CRASH_NULL: u64 = 21;
const CRASH_EXEC_DATA: u64 = 22;
const CRASH_UNDEFINED: u64 = 23;
const CRASH_PANIC: u64 = 24;
const CRASH_STACK: u64 = 25;
const CRASH_WRITE_CODE: u64 = 26;

/// Bytes the kernel adds in front of a payload when copying a message out.
const HEADER: usize = 24;
const RETRY_LIMIT: u32 = 50_000_000;
const MAXIMUM_SENDERS: usize = 16;
const SELF_QUEUE_LIMIT: u64 = 100_000;
const SELF_CYCLES: u64 = 24;
const FUZZ_ROUNDS: u32 = 2048;

/// Cross-page input and output avoid depending on a large EL0 CPU stack.
#[repr(align(4096))]
struct Pages([u8; 8192]);
static mut INPUT: Pages = Pages([0; 8192]);
static mut OUTPUT: Pages = Pages([0; 8192]);

struct Bootstrap {
    peer: LocalAddress,
    role: u64,
    scenario: u64,
    seed: u64,
    param: u64,
}

struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        Self(seed | 1)
    }

    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }
}

/// Stops through an actor-local fault; never reports successful completion.
fn fail() -> ! {
    // SAFETY: Intentional illegal instruction tests the actor-local failure path.
    unsafe {
        core::arch::asm!("brk #0", options(noreturn));
    }
}

/// Relinquishes this actor permanently.
fn finish() -> ! {
    // SAFETY: FinishActor relinquishes this actor permanently and does not return.
    unsafe {
        core::arch::asm!("svc #0", in("x8") SystemCall::FinishActor.number(), options(noreturn));
    }
}

/// Test-only raw syscall for deliberately invalid pointers and scalar options.
unsafe fn call(number: SystemCall, arguments: [u64; 6]) -> (u64, u64) {
    let [mut status, mut value, two, three, four, five] = arguments;
    // SAFETY: This test intentionally submits invalid user addresses; the kernel
    // must validate them before access. Buffers used in successful calls are live.
    unsafe {
        core::arch::asm!("svc #0", in("x8") number.number(), inout("x0") status,
        inout("x1") value, in("x2") two, in("x3") three, in("x4") four, in("x5") five,
        options(nostack));
    }
    (status, value)
}

/// Both static pages as byte slices. Call at most once per actor.
unsafe fn pages() -> (&'static mut [u8], &'static mut [u8]) {
    // SAFETY: Each isolated actor owns a private mapping of these statics and each
    // scenario calls this once, so exactly one mutable reference exists per region.
    unsafe {
        (
            core::slice::from_raw_parts_mut((&raw mut INPUT.0).cast::<u8>(), 8192),
            core::slice::from_raw_parts_mut((&raw mut OUTPUT.0).cast::<u8>(), 8192),
        )
    }
}

/// Reads the `index`th little-endian u64 of a payload.
fn word(bytes: &[u8], index: usize) -> u64 {
    let start = index * 8;
    let slice = bytes.get(start..start + 8).unwrap_or_else(|| fail());
    u64::from_le_bytes(slice.try_into().unwrap_or_else(|_| fail()))
}

/// Sends, retrying while the receiver's inbox or the pool is full.
fn send_retry(destination: LocalAddress, tag: Tag, correlation: u64, payload: &[u8]) {
    for _ in 0..RETRY_LIMIT {
        if inbox::tell_actor(destination, tag, correlation, payload).is_ok() {
            return;
        }
        spin_loop();
    }
    fail()
}

fn read_bootstrap(record: &mut [u8]) -> Bootstrap {
    let message = inbox::await_matching(
        MessageSelector {
            tag: Some(0),
            ..MessageSelector::default()
        },
        record,
    )
    .unwrap_or_else(|_| fail());

    let payload = message.payload;
    let packed = word(payload, 0);

    Bootstrap {
        peer: LocalAddress::new(
            u32::try_from(packed & u64::from(u32::MAX)).unwrap_or_else(|_| fail()),
            u32::try_from(packed >> 32).unwrap_or_else(|_| fail()),
        ),
        role: word(payload, 1),
        scenario: word(payload, 2),
        seed: word(payload, 3),
        param: word(payload, 4),
    }
}

// ---------------------------------------------------------------------------
// Healthy scenarios
// ---------------------------------------------------------------------------

/// The original copy validation: checked copies across a page boundary.
fn copy_validation(own: LocalAddress) {
    // SAFETY: called once; see `pages`.
    let (input_page, output_page) = unsafe { pages() };
    let input = &mut input_page[4090..4122];
    let output = &mut output_page[4090..4154];

    input.fill(0x5a);
    inbox::tell_actor(own, 10, 99, input).unwrap_or_else(|_| fail());

    let own_selector = MessageSelector {
        sender: Some(own),
        tag: Some(10),
        correlation: Some(99),
    };

    let small = inbox::receive_message(own_selector, &mut []).unwrap_err();
    if small.status != SystemCallStatus::BufferTooSmall || small.required_byte_count != 56 {
        fail();
    }

    // SAFETY: Deliberately invalid output pointer must be rejected without consuming.
    let invalid = unsafe { call(SystemCall::ReceiveMessage, [u64::MAX - 20, 56, 2, 0, 10, 0]) };
    if invalid.0 != SystemCallStatus::InvalidUserMemory.number() {
        fail();
    }

    let message = inbox::receive_message(own_selector, output).unwrap_or_else(|_| fail());
    if message.payload != input || message.sender != own {
        fail();
    }

    // SAFETY: Poll and unsupported timeout checks use scalar-only arguments.
    let (poll, finite) = unsafe {
        (
            call(SystemCall::AwaitInbox, [2, 0, 999, 0, 0, 0]),
            call(SystemCall::AwaitInbox, [0, 0, 0, 0, 1, 0]),
        )
    };
    if poll.0 != SystemCallStatus::TimedOut.number()
        || finite.0 != SystemCallStatus::UnsupportedOperation.number()
    {
        fail();
    }
}

/// Near-miss mail parked in the inbox for the whole exchange. Each entry shares the
/// tag or correlation of a live selector but comes from the wrong sender (self).
const NOISE: [(Tag, u64, &[u8]); 4] = [
    (42, 0, b"near-miss-first"),
    (42, 5, b"near-miss-mid"),
    (43, 0, b"wrong-tag"),
    (99, 0, b"left queued for exit cleanup"),
];

/// Copy validation, then 128 selective exchanges with noise in the queue.
/// `param != 0` makes the actor die at that sequence, orphaning its peer.
fn ping_pong(b: &Bootstrap, record: &mut [u8]) {
    let own = inbox::current_address().unwrap_or_else(|_| fail());
    let (peer, role, die_after) = (b.peer, b.role, b.param);

    copy_validation(own);

    for (tag, correlation, body) in NOISE {
        inbox::tell_actor(own, tag, correlation, body).unwrap_or_else(|_| fail());
    }

    for sequence in 0..128u64 {
        let selector = MessageSelector {
            sender: Some(peer),
            tag: Some(42),
            correlation: Some(sequence),
        };
        if role == 0 {
            inbox::tell_actor(peer, 42, sequence, b"ping").unwrap_or_else(|_| fail());
            if die_after != 0 && sequence == die_after {
                fail(); // dies with a request in flight
            }
        }
        let message = inbox::await_matching(selector, record).unwrap_or_else(|_| fail());
        let expected: &[u8] = if role == 0 { b"pong" } else { b"ping" };
        if message.sender != peer || message.payload != expected {
            fail();
        }
        if role == 1 {
            if die_after != 0 && sequence == die_after {
                fail(); // dies without replying
            }
            inbox::tell_actor(peer, 42, sequence, b"pong").unwrap_or_else(|_| fail());
        }
    }

    // Noise must have survived all exchanges. Receive it out of order (last first).
    // The tag-99 entry is deliberately left for exit cleanup.
    for (tag, correlation, body) in [NOISE[2], NOISE[1], NOISE[0]] {
        let selector = MessageSelector {
            sender: Some(own),
            tag: Some(tag),
            correlation: Some(correlation),
        };
        let message = inbox::receive_message(selector, record).unwrap_or_else(|_| fail());
        if message.sender != own || message.payload != body {
            fail();
        }
    }
}

/// Floods the collector with variable-size, self-describing messages.
fn collector_sender(b: &Bootstrap) {
    let mut payload = [0u8; 64];
    for sequence in 0..b.param {
        let length = 16 + (sequence % 48) as usize;
        payload[..8].copy_from_slice(&b.role.to_le_bytes());
        payload[8..16].copy_from_slice(&sequence.to_le_bytes());
        payload[16..length].fill(sequence as u8);
        send_retry(b.peer, 8, sequence, &payload[..length]);
    }
}

/// Receives from many senders at once and checks per-sender FIFO order and content.
fn collector(b: &Bootstrap, record: &mut [u8]) {
    let senders = usize::try_from(b.role).unwrap_or_else(|_| fail());
    if senders > MAXIMUM_SENDERS {
        fail();
    }
    let mut next = [0u64; MAXIMUM_SENDERS];

    for _ in 0..(senders as u64 * b.param) {
        let message = inbox::await_matching(
            MessageSelector {
                tag: Some(8),
                ..MessageSelector::default()
            },
            record,
        )
        .unwrap_or_else(|_| fail());
        let payload = message.payload;

        let index = usize::try_from(word(payload, 0)).unwrap_or_else(|_| fail());
        let sequence = word(payload, 1);
        if index >= senders || sequence != next[index] {
            fail(); // reordered, duplicated or lost within one sender
        }
        next[index] += 1;

        if payload.len() != 16 + (sequence % 48) as usize
            || payload[16..].iter().any(|&byte| byte != sequence as u8)
        {
            fail();
        }
    }

    if next[..senders].iter().any(|&count| count != b.param) {
        fail();
    }
}

/// Drains `count` self-sent tag-77 messages in forward, reverse or interleaved order.
fn drain_self(own: LocalAddress, count: u64, mode: u64) {
    let mut buffer = [0u8; 64];
    let mut take = |index: u64| {
        let selector = MessageSelector {
            sender: Some(own),
            tag: Some(77),
            correlation: Some(index),
        };
        let message = inbox::receive_message(selector, &mut buffer).unwrap_or_else(|_| fail());
        if message.sender != own || message.payload != &index.to_le_bytes()[..] {
            fail();
        }
    };

    match mode {
        0 => (0..count).for_each(&mut take),
        1 => (0..count).rev().for_each(&mut take),
        _ => {
            (0..count).step_by(2).for_each(&mut take);
            (1..count)
                .filter(|index| index % 2 == 1)
                .rev()
                .for_each(&mut take);
        }
    }

    let leftover = MessageSelector {
        tag: Some(77),
        ..MessageSelector::default()
    };
    if inbox::receive_message(leftover, &mut buffer).is_ok() {
        fail();
    }
}

/// Fills its own inbox until the kernel refuses, drains it in different orders and
/// repeats. A shrinking capacity across cycles points to a leaked pool slot.
/// `param != 0` demands identical capacity every cycle (only valid on an idle system).
fn self_cycle(b: &Bootstrap) {
    let own = inbox::current_address().unwrap_or_else(|_| fail());
    let strict = b.param != 0;
    let mut first = 0u64;
    let mut empty_rounds = 0u32;

    let mut cycle = 0;
    while cycle < SELF_CYCLES {
        let mut count = 0u64;
        loop {
            if count >= SELF_QUEUE_LIMIT {
                fail(); // the inbox has no bound at all
            }
            if inbox::tell_actor(own, 77, count, &count.to_le_bytes()).is_err() {
                break;
            }
            count += 1;
        }

        if count == 0 {
            // The shared pool may be exhausted by other actors; retry for a while.
            empty_rounds += 1;
            if empty_rounds > 10_000 {
                fail();
            }
            spin_loop();
            continue;
        }

        if cycle == 0 {
            first = count;
        } else if (strict && count != first) || count * 2 < first {
            fail();
        }

        if strict {
            for _ in 0..4 {
                if inbox::tell_actor(own, 77, count, b"overflow").is_ok() {
                    fail();
                }
            }
        }

        drain_self(own, count, cycle % 3);
        cycle += 1;
    }
}

/// Sends payloads of many sizes from page-crossing offsets to itself. A short buffer
/// must report the exact size without consuming; a rejected size must stay rejected.
fn size_probe() {
    const SIZES: &[usize] = &[
        0, 1, 2, 3, 7, 8, 15, 16, 31, 32, 63, 64, 65, 127, 128, 129, 255, 256, 257, 511, 512, 513,
        1023, 1024, 1025, 2047, 2048, 2049, 4095, 4096, 4097, 8000, 8168,
    ];

    let own = inbox::current_address().unwrap_or_else(|_| fail());
    // SAFETY: called once; see `pages`.
    let (input, output) = unsafe { pages() };
    let mut rejected = false;

    for &size in SIZES {
        let offset = if size <= 4096 {
            4096 - size / 2
        } else {
            8192 - size
        };
        for (index, byte) in input[offset..offset + size].iter_mut().enumerate() {
            *byte = (index as u8).wrapping_mul(31).wrapping_add(size as u8);
        }

        let selector = MessageSelector {
            sender: Some(own),
            tag: Some(5),
            correlation: Some(size as u64),
        };

        if inbox::tell_actor(own, 5, size as u64, &input[offset..offset + size]).is_err() {
            rejected = true;
            continue;
        }
        if rejected {
            fail(); // a larger payload was accepted after a smaller one was refused
        }

        let required = size + HEADER;
        let out = if required <= 4096 {
            4096 - required / 2
        } else {
            8192 - required
        };

        if size > 0 {
            let short =
                inbox::receive_message(selector, &mut output[out..out + required - 1]).unwrap_err();
            if short.status != SystemCallStatus::BufferTooSmall
                || short.required_byte_count as usize != required
            {
                fail();
            }
        }

        let message = inbox::receive_message(selector, &mut output[out..out + required])
            .unwrap_or_else(|_| fail());
        if message.sender != own || message.payload != &input[offset..offset + size] {
            fail();
        }
    }

    let leftover = MessageSelector {
        tag: Some(5),
        ..MessageSelector::default()
    };
    if inbox::receive_message(leftover, &mut output[..64]).is_ok() {
        fail();
    }
}

/// Throws hostile syscall arguments at the kernel. Invalid user memory must be
/// rejected without consuming the matching message, unsupported timeouts must be
/// refused, and nothing may take the kernel down.
fn syscall_fuzz(b: &Bootstrap) {
    let own = inbox::current_address().unwrap_or_else(|_| fail());
    let mut rng = Rng::new(b.seed);
    // SAFETY: only the address is taken; the buffer is not accessed here.
    let output_base = unsafe { (&raw mut OUTPUT.0).cast::<u8>() as usize as u64 };

    inbox::tell_actor(own, 10, 99, &[7u8; 8]).unwrap_or_else(|_| fail());

    for _ in 0..FUZZ_ROUNDS {
        let length = 32 + rng.next() % 64;
        let (pointer, length) = match rng.next() % 3 {
            // Pointer plus length wraps the address space.
            0 => (u64::MAX - rng.next() % 64, length),
            // Kernel half of the address space.
            1 => (
                0xffff_0000_0000_0000 | (rng.next() & 0x0000_ffff_ffff_fff8),
                length,
            ),
            // Valid base, absurd length.
            _ => (output_base, u64::MAX - rng.next() % 4096),
        };
        // SAFETY: invalid pointers are the point; the kernel must validate them.
        let result = unsafe { call(SystemCall::ReceiveMessage, [pointer, length, 2, 0, 10, 0]) };
        if result.0 != SystemCallStatus::InvalidUserMemory.number() {
            fail();
        }

        // SAFETY: scalar-only arguments.
        let (poll, finite) = unsafe {
            (
                call(SystemCall::AwaitInbox, [2, 0, 999, 0, 0, 0]),
                call(SystemCall::AwaitInbox, [0, 0, 0, 0, rng.next() | 1, 0]),
            )
        };
        if poll.0 != SystemCallStatus::TimedOut.number()
            || finite.0 != SystemCallStatus::UnsupportedOperation.number()
        {
            fail();
        }
    }

    // None of the rejected calls may have consumed the sentinel.
    let selector = MessageSelector {
        sender: Some(own),
        tag: Some(10),
        correlation: Some(99),
    };
    let mut buffer = [0u8; 64];
    let message = inbox::receive_message(selector, &mut buffer).unwrap_or_else(|_| fail());
    if message.payload != [7u8; 8] {
        fail();
    }
}

/// Serves `param` requests from any client. Replies to dead clients are allowed to fail.
fn server(b: &Bootstrap, record: &mut [u8]) {
    for _ in 0..b.param {
        let request = inbox::await_matching(
            MessageSelector {
                tag: Some(100),
                ..MessageSelector::default()
            },
            record,
        )
        .unwrap_or_else(|_| fail());

        let client = request.sender;
        let sequence = word(request.payload, 0);
        for _ in 0..200_000 {
            if inbox::tell_actor(client, 101, sequence, request.payload).is_ok() {
                break;
            }
            spin_loop();
        }
    }
}

/// Builds the request payload for `sequence` into `payload`; returns its length.
fn build_request(payload: &mut [u8; 48], sequence: u64, index: u64) -> usize {
    let length = 8 + ((sequence * 7 + index) % 40) as usize;
    payload[..8].copy_from_slice(&sequence.to_le_bytes());
    for (position, byte) in payload[8..length].iter_mut().enumerate() {
        *byte = (sequence as u8) ^ (position as u8) ^ (index as u8);
    }
    length
}

/// Request/reply client with selective await on (server, tag, correlation).
fn client(b: &Bootstrap, record: &mut [u8]) {
    let mut payload = [0u8; 48];
    for sequence in 0..b.param {
        let length = build_request(&mut payload, sequence, b.role);
        send_retry(b.peer, 100, sequence, &payload[..length]);

        let selector = MessageSelector {
            sender: Some(b.peer),
            tag: Some(101),
            correlation: Some(sequence),
        };
        let reply = inbox::await_matching(selector, record).unwrap_or_else(|_| fail());
        if reply.sender != b.peer || reply.payload != &payload[..length] {
            fail();
        }
    }
}

/// Sends one request, then dies before the reply arrives.
fn client_crash(b: &Bootstrap) -> ! {
    let mut payload = [0u8; 48];
    let length = build_request(&mut payload, 0, b.role);
    send_retry(b.peer, 100, 0, &payload[..length]);
    fail()
}

/// Burns CPU. Preemption must let every other actor keep making progress.
fn spin_hog(b: &Bootstrap) {
    let iterations = if b.param == 0 { 100_000_000 } else { b.param };
    let mut accumulator = 0u64;
    for index in 0..iterations {
        accumulator = black_box(accumulator.wrapping_add(index));
    }
    black_box(accumulator);
}

/// Spams an address that is dead or dying. Stale addresses must fail cleanly.
fn dead_letter(b: &Bootstrap) {
    for index in 0..4096u64 {
        let _ = inbox::tell_actor(b.peer, 1, index, b"into the void");
        if index % 256 == 0 {
            spin_loop();
        }
    }
}

/// Fills a peer that never receives until the kernel says no. The bound must exist
/// and must keep holding.
fn fill_victim(b: &Bootstrap) {
    let mut sent = 0u64;
    loop {
        if sent >= SELF_QUEUE_LIMIT {
            fail(); // unbounded inbox
        }
        if inbox::tell_actor(b.peer, 9, sent, &[0xab; 32]).is_err() {
            break;
        }
        sent += 1;
    }
}

/// Waits for a message nobody sends. The actor is reclaimed with its inbox full.
fn block_forever(record: &mut [u8]) -> ! {
    let _ = inbox::await_matching(
        MessageSelector {
            tag: Some(0xdead),
            ..MessageSelector::default()
        },
        record,
    );
    fail()
}

// ---------------------------------------------------------------------------
// Fault scenarios
// ---------------------------------------------------------------------------

fn crash_null() -> ! {
    // SAFETY: deliberate fault. The kernel must contain it to this actor.
    unsafe {
        core::ptr::write_volatile(core::ptr::without_provenance_mut::<u64>(0x10), 1);
    }
    fail()
}

fn crash_exec_data() -> ! {
    // SAFETY: deliberate permission fault: jumping into non-executable data.
    unsafe {
        let address = (&raw mut INPUT.0).cast::<u8>() as usize;
        core::arch::asm!("br {0}", in(reg) address, options(noreturn));
    }
}

fn crash_undefined() -> ! {
    // SAFETY: deliberate undefined instruction.
    unsafe {
        core::arch::asm!("udf #0", options(noreturn));
    }
}

fn crash_write_code() -> ! {
    // SAFETY: deliberate store to executable memory; write-xor-execute must refuse it.
    unsafe {
        core::ptr::write_volatile(crash_write_code as usize as *mut u8, 0);
    }
    fail()
}

fn recurse(depth: u64) -> u64 {
    let pad = black_box([depth as u8; 256]);
    if black_box(depth) == u64::MAX {
        return 0;
    }
    recurse(depth + 1).wrapping_add(u64::from(pad[0]))
}

fn crash_stack() -> ! {
    black_box(recurse(0));
    fail()
}

/// Runs the scenario named in the bootstrap message, then exits.
pub fn run() -> ! {
    let mut record = [0u8; 128];
    let b = read_bootstrap(&mut record);

    match b.scenario {
        PING_PONG => ping_pong(&b, &mut record),
        COLLECTOR_SENDER => collector_sender(&b),
        COLLECTOR => collector(&b, &mut record),
        SELF_CYCLE => self_cycle(&b),
        SIZE_PROBE => size_probe(),
        SYSCALL_FUZZ => syscall_fuzz(&b),
        SERVER => server(&b, &mut record),
        CLIENT => client(&b, &mut record),
        CLIENT_CRASH => client_crash(&b),
        QUICK_EXIT => {}
        SPIN_HOG => spin_hog(&b),
        DEAD_LETTER => dead_letter(&b),
        FILL_VICTIM => fill_victim(&b),
        BLOCK_FOREVER => block_forever(&mut record),
        CRASH_BRK => fail(),
        CRASH_NULL => crash_null(),
        CRASH_EXEC_DATA => crash_exec_data(),
        CRASH_UNDEFINED => crash_undefined(),
        CRASH_PANIC => {
            let nothing: Option<u8> = black_box(None);
            nothing.expect("deliberate panic");
        }
        CRASH_STACK => crash_stack(),
        CRASH_WRITE_CODE => crash_write_code(),
        _ => fail(),
    }

    finish()
}

/// Reports an actor-local failure instead of spinning indefinitely after a panic.
#[panic_handler]
fn on_panic(_information: &core::panic::PanicInfo<'_>) -> ! {
    fail()
}

/// Starts the stress program after the kernel enters this actor at EL0.
#[unsafe(no_mangle)]
pub extern "C" fn _start() -> ! {
    run()
}
