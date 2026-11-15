//! Dedicated EL0 actor residency audit.
//!
//! Spawns 1,000 independent actors, sends each a `BlockForever` assignment,
//! and proves that it consumed its bootstrap and parked with no queued messages.
//! Once all actors are resident, the audit reclaims their physical frames and
//! then exercises wake-versus-termination races between EL0 senders and kernel cleanup.
//! Contested rounds place the sender on a secondary core and vary the
//! termination delay to exercise both race outcomes.
//! The final sustained-load phase floods independent collectors across several
//! full actor lifetimes and checks reclamation after every wave.

mod load;

use core::hint::spin_loop;

use foundation::{
    actor::{Actor, Image},
    address::LocalAddress,
    name::Name,
    stress::{BOOTSTRAP_TAG, Bootstrap, RACE_START_TAG, Scenario},
};

use crate::{
    actors::{
        control_block::State,
        divider::{default_configuration, system_limit},
        inbox::MessageMetadata,
        lifecycle::{WaitingTerminationError, terminate_waiting_actor},
        message_storage::MESSAGE_POOL,
        snapshot, table,
    },
    architecture::aarch64::{early_console::EarlyConsole, power_interface, processor, time},
    information,
    memory::frame_allocator::with_frame_allocator,
    messaging::delivery::tell_actor,
    scheduling::core_local::{CoreLocal, core_locals, current_core_local},
};

/// Number of EL0 actors which must be alive and waiting at the same time.
const TARGET_ACTOR_COUNT: usize = 1_000;
/// Number of successful actor park operations between progress reports.
const REPORT_INTERVAL: usize = 50;
/// Maximum time allowed for an individual actor to consume its bootstrap and park.
const ACTOR_WAIT_TIMEOUT_MILLISECONDS: u64 = 3_000;
/// Three test modes: termination-first, wake-first, and concurrent competition.
const RACE_ROUND_COUNT: usize = 192;
/// Number of complete rounds between race progress reports.
const RACE_REPORT_INTERVAL: usize = 32;
/// Delay sweep between releasing a remote sender and attempting termination.
/// The first delays favor termination; later delays allow a remote wake to win.
const CONTESTED_TERMINATION_DELAYS_MILLISECONDS: [u64; 8] = [0, 1, 2, 5, 10, 20, 35, 50];

/// Stress-enabled Genesis ELF (the same binary used by the existing stress suite).
static GENESIS_PROGRAM: &[u8] = include_bytes!(env!("GENESIS_ELF_PATH"));

/// Sends one bootstrap assignment. Pool pressure is unexpected: the previous
/// actor must have consumed its bootstrap before this function runs again.
fn assign_block_forever(address: LocalAddress) {
    let message = Bootstrap {
        peer: address.to_packed(),
        role: 0,
        scenario: Scenario::BlockForever,
        seed: 0,
        param: 0,
    }
    .encode();
    let metadata = MessageMetadata {
        sender: address,
        tag: BOOTSTRAP_TAG,
        correlation: 0,
    };

    if let Err(error) = tell_actor(address, metadata, &message) {
        crate::error!(
            EarlyConsole,
            "Residency bootstrap rejected for {address}: {error:?}"
        );
        power_interface::halt_machine();
    }
}

/// Returns true only when this exact actor incarnation has consumed its
/// bootstrap message and parked with a completely empty inbox.
fn is_parked_after_bootstrap(address: LocalAddress) -> bool {
    table::with_control_block(address.slot_index() as usize, |actor| {
        actor.generation_counter == address.generation_counter()
            && actor.state == State::Waiting
            && !actor.queued
            && actor.address_space.is_some()
            && actor
                .inbox
                .as_ref()
                .is_some_and(|inbox| inbox.message_count() == 0)
    })
    .unwrap_or(false)
}

/// Waits for one actor's confirmed post-bootstrap parked state.
fn await_parked(address: LocalAddress) {
    let tick_count =
        time::read_counter_frequency().saturating_mul(ACTOR_WAIT_TIMEOUT_MILLISECONDS) / 1_000;
    assert!(
        tick_count > 0,
        "The residency timeout requires a running counter"
    );
    let deadline = time::read_counter_value().saturating_add(tick_count);

    while !is_parked_after_bootstrap(address) {
        assert!(
            time::read_counter_value() < deadline,
            "Residency actor {address} did not consume its bootstrap and enter Waiting"
        );
        processor::wait_for_event();
    }
}

/// Checks stable, independently sampled counters after every actor has parked.
fn verify_counts(expected_actor_count: usize) {
    let (occupied, waiting) = snapshot::current();
    assert_eq!(
        occupied, expected_actor_count,
        "Residency occupied count drift"
    );
    assert_eq!(
        waiting, expected_actor_count,
        "Residency waiting count drift"
    );

    let pool = MESSAGE_POOL.lock().statistics();
    assert_eq!(pool.allocated, 0, "Residency bootstrap message leak");
    assert_eq!(pool.charged_bytes, 0, "Residency bootstrap charge leak");
}

/// Checks the actual actor table rather than trusting only aggregate counters.
fn verify_actor_table() {
    let mut verified_actors = 0;

    for slot_index in 0..table::actor_table_length() {
        let valid_resident = table::with_control_block(slot_index, |actor| match actor.state {
            State::Free => None,
            State::Waiting => Some(
                !actor.queued
                    && actor.address_space.is_some()
                    && actor
                        .inbox
                        .as_ref()
                        .is_some_and(|inbox| inbox.message_count() == 0),
            ),
            _ => Some(false),
        })
        .unwrap_or_else(|| {
            crate::error!(
                EarlyConsole,
                "Residency actor slot {slot_index} disappeared"
            );
            power_interface::halt_machine()
        });

        if let Some(valid_resident) = valid_resident {
            assert!(
                valid_resident,
                "Residency actor in slot {slot_index} is not parked"
            );
            verified_actors += 1;
        }
    }

    assert_eq!(
        verified_actors, TARGET_ACTOR_COUNT,
        "Residency audit found the wrong number of live actors"
    );
}

/// Terminates every resident actor and verifies generation safety and zero resource drift.
fn reclaim_resident_population(baseline_free_frame_count: usize) {
    let mut reclaimed_actor_count = 0_usize;

    for slot_index in 0..table::actor_table_length() {
        let address = table::with_control_block(slot_index, |actor| {
            if actor.state != State::Waiting {
                return None;
            }
            let slot_number = u32::try_from(slot_index).ok()?;
            Some(LocalAddress::new(slot_number, actor.generation_counter))
        })
        .flatten();

        let Some(address) = address else {
            continue;
        };

        if let Err(error) = terminate_waiting_actor(address) {
            crate::error!(
                EarlyConsole,
                "Residency actor {address} could not be reclaimed: {error:?}"
            );
            power_interface::halt_machine();
        }

        // No stale actor address may receive a message after recycling.
        let metadata = MessageMetadata {
            sender: address,
            tag: BOOTSTRAP_TAG,
            correlation: 0,
        };
        assert!(
            tell_actor(address, metadata, &[]).is_err(),
            "Recycled actor {address} accepted a stale message"
        );

        reclaimed_actor_count += 1;
        if reclaimed_actor_count % REPORT_INTERVAL == 0 {
            information!(
                EarlyConsole,
                "Residency reclamation: {reclaimed_actor_count}/{TARGET_ACTOR_COUNT} actors"
            );
        }
    }

    assert_eq!(
        reclaimed_actor_count, TARGET_ACTOR_COUNT,
        "Residency reclamation did not process every live actor"
    );
    verify_counts(0);

    // Check every slot, not just the slots selected by the termination pass.
    for slot_index in 0..table::actor_table_length() {
        let fully_recycled = table::with_control_block(slot_index, |actor| {
            actor.state == State::Free
                && !actor.queued
                && actor.inbox.is_none()
                && actor.address_space.is_none()
                && actor.configuration.is_none()
                && actor.parent_address.is_none()
                && actor.saved_frame_address == 0
                && actor.pending_message_releases == 0
                && actor.dynamic_memory_page_count == 0
                && actor.exit_cause.is_none()
                && actor.address_space_identifier == 0
                && actor.last_core_index.is_none()
                && actor.name == Name::UNNAMED
        })
        .unwrap_or(false);
        assert!(
            fully_recycled,
            "Residency slot {slot_index} was not fully reclaimed"
        );
    }

    let final_free_frame_count = with_frame_allocator(|allocator| allocator.free_frame_count())
        .unwrap_or_else(|| {
            crate::error!(
                EarlyConsole,
                "Frame allocator unavailable after residency reclamation"
            );
            power_interface::halt_machine()
        });
    assert_eq!(
        final_free_frame_count, baseline_free_frame_count,
        "Residency left allocated physical frames after all actors exited"
    );

    information!(
        EarlyConsole,
        "Residency reclamation passed: {TARGET_ACTOR_COUNT} actors recycled, all frames and message charges restored"
    );
}

/// Spawns a single EL0 actor, failing with the precise creation error.
fn spawn_race_actor(round: usize) -> LocalAddress {
    table::enqueue(Actor::new(
        Name::UNNAMED,
        default_configuration(),
        Image::Elf(GENESIS_PROGRAM),
    ))
    .unwrap_or_else(|error| {
        crate::error!(
            EarlyConsole,
            "Race round {round}: actor spawn failed: {error:?}"
        );
        power_interface::halt_machine()
    })
}

/// Installs a stress assignment with a caller-selected peer and scenario.
fn assign_race_scenario(actor_address: LocalAddress, peer: LocalAddress, scenario: Scenario) {
    let message = Bootstrap {
        peer: peer.to_packed(),
        role: 0,
        scenario,
        seed: 0,
        param: 0,
    }
    .encode();
    let metadata = MessageMetadata {
        sender: actor_address,
        tag: BOOTSTRAP_TAG,
        correlation: 0,
    };
    if let Err(error) = tell_actor(actor_address, metadata, &message) {
        crate::error!(
            EarlyConsole,
            "Race bootstrap rejected for {actor_address} ({scenario:?}): {error:?}"
        );
        power_interface::halt_machine();
    }
}

/// Sends the start signal to a parked EL0 sender, enabling the competing send.
fn release_race_sender(sender_address: LocalAddress) {
    let metadata = MessageMetadata {
        sender: sender_address,
        tag: RACE_START_TAG,
        correlation: 0,
    };
    if let Err(error) = tell_actor(sender_address, metadata, &[]) {
        crate::error!(
            EarlyConsole,
            "Race start rejected for {sender_address}: {error:?}"
        );
        power_interface::halt_machine();
    }
}

/// Waits until an actor has exited its parked state or its slot has been recycled.
fn await_wake_transition(address: LocalAddress) {
    let deadline = time::read_counter_value().saturating_add(
        time::read_counter_frequency().saturating_mul(ACTOR_WAIT_TIMEOUT_MILLISECONDS) / 1_000,
    );
    loop {
        let still_waiting = table::with_control_block(address.slot_index() as usize, |actor| {
            actor.generation_counter == address.generation_counter()
                && actor.state == State::Waiting
        })
        .unwrap_or(false);
        if !still_waiting {
            return;
        }
        assert!(
            time::read_counter_value() < deadline,
            "Wake-first race: {address} never left Waiting"
        );
        processor::wait_for_event();
    }
}

/// Confirms that an exact incarnation has been completely recycled.
fn await_recycled(address: LocalAddress) {
    let deadline = time::read_counter_value().saturating_add(
        time::read_counter_frequency().saturating_mul(ACTOR_WAIT_TIMEOUT_MILLISECONDS) / 1_000,
    );
    loop {
        let recycled = table::with_control_block(address.slot_index() as usize, |actor| {
            actor.generation_counter == address.generation_counter().wrapping_add(1)
                && actor.state == State::Free
                && !actor.queued
                && actor.inbox.is_none()
                && actor.address_space.is_none()
        })
        .unwrap_or(false);
        if recycled {
            break;
        }
        assert!(
            time::read_counter_value() < deadline,
            "Race actor {address} was not recycled"
        );
        processor::wait_for_event();
    }

    let metadata = MessageMetadata {
        sender: address,
        tag: RACE_START_TAG,
        correlation: 0,
    };
    assert!(
        tell_actor(address, metadata, &[]).is_err(),
        "A recycled actor {address} accepted a stale message"
    );
}

/// Selects an online secondary core for a contested sender, rotating across cores.
fn secondary_core_for_round(round: usize) -> &'static CoreLocal {
    let boot_core_index = current_core_local()
        .unwrap_or_else(|| power_interface::halt_machine())
        .core_index();
    let online_secondary_count = core_locals()
        .iter()
        .filter(|core| core.is_online() && core.core_index() != boot_core_index)
        .count();
    assert!(
        online_secondary_count > 0,
        "Contested residency races require at least two online cores"
    );

    core_locals()
        .iter()
        .filter(|core| core.is_online() && core.core_index() != boot_core_index)
        .nth((round / 3) % online_secondary_count)
        .unwrap_or_else(|| power_interface::halt_machine())
}

/// Gives a parked test sender a remote wake-placement hint.
///
/// This modifies a scheduler hint only in the residency fixture, never a
/// runnable queue entry. The actual remote dispatch is independently checked.
fn prepare_remote_sender(sender_address: LocalAddress, remote_core: &CoreLocal) -> u64 {
    let configured = table::with_control_block(sender_address.slot_index() as usize, |actor| {
        if actor.generation_counter != sender_address.generation_counter()
            || actor.state != State::Waiting
            || actor.queued
        {
            return false;
        }
        actor.last_core_index = Some(remote_core.core_index());
        true
    })
    .unwrap_or(false);
    assert!(
        configured,
        "Cannot prepare parked sender {sender_address} for remote wake"
    );

    remote_core.scheduler_statistics().actor_dispatch_count
}

/// Allows remote send and kernel termination to overlap at different delays.
fn delay_contested_termination(round: usize) {
    let delay_index = (round / 3) % CONTESTED_TERMINATION_DELAYS_MILLISECONDS.len();
    let milliseconds = CONTESTED_TERMINATION_DELAYS_MILLISECONDS
        .get(delay_index)
        .copied()
        .unwrap_or_else(|| power_interface::halt_machine());
    if milliseconds == 0 {
        return;
    }

    let duration = time::read_counter_frequency().saturating_mul(milliseconds) / 1_000;
    assert!(
        duration != 0,
        "Contested race delay is below timer resolution"
    );
    let deadline = time::read_counter_value().saturating_add(duration);
    while time::read_counter_value() < deadline {
        spin_loop();
    }
}

/// A full round starts with two actors parked on unrelated tags.
///
/// The sender only runs its application send after a kernel start message.
/// Termination-first and wake-first supply deterministic control cases, while
/// the third case launches the sender and races its wake against termination.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RaceOutcome {
    /// The kernel closed the receiver inbox while it was still parked.
    Termination,
    /// The sender woke or completed the receiver before termination.
    Wake,
}

/// Starts two independently scheduled EL0 actors and settles one race.
fn run_race_round(round: usize) -> RaceOutcome {
    let receiver_address = spawn_race_actor(round);
    assign_race_scenario(receiver_address, receiver_address, Scenario::RaceReceiver);
    await_parked(receiver_address);

    let sender_address = spawn_race_actor(round);
    assign_race_scenario(sender_address, receiver_address, Scenario::RaceSender);
    await_parked(sender_address);

    // In contested rounds the sender must run on another core. This avoids
    // same-core rescheduling always deciding the outcome before termination.
    let remote_core = (round % 3 == 2).then(|| secondary_core_for_round(round));
    let remote_dispatch_before =
        remote_core.map(|core| prepare_remote_sender(sender_address, core));

    let outcome = match round % 3 {
        0 => {
            // Termination first: the late sender must reject the stale address.
            assert_eq!(
                terminate_waiting_actor(receiver_address),
                Ok(()),
                "Termination-first round {round} did not reclaim its receiver"
            );
            release_race_sender(sender_address);
            RaceOutcome::Termination
        }
        1 => {
            // Wakeup first: parked-only termination must refuse a moving actor.
            release_race_sender(sender_address);
            await_wake_transition(receiver_address);
            assert!(
                matches!(
                    terminate_waiting_actor(receiver_address),
                    Err(WaitingTerminationError::NotWaiting
                        | WaitingTerminationError::InvalidAddress)
                ),
                "Wake-first round {round} unexpectedly terminated the awakened receiver"
            );
            RaceOutcome::Wake
        }
        _ => {
            // Send and terminate are now eligible to run on separate cores.
            // Different delays sweep the timing of the competing lock claims.
            release_race_sender(sender_address);
            delay_contested_termination(round);
            match terminate_waiting_actor(receiver_address) {
                Ok(()) => RaceOutcome::Termination,
                Err(
                    WaitingTerminationError::NotWaiting | WaitingTerminationError::InvalidAddress,
                ) => RaceOutcome::Wake,
                Err(error) => {
                    crate::error!(
                        EarlyConsole,
                        "Race round {round}: unsafe termination result: {error:?}"
                    );
                    power_interface::halt_machine();
                }
            }
        }
    };

    // Both the kernel-terminated and self-exiting paths must reclaim completely.
    await_recycled(receiver_address);
    await_recycled(sender_address);
    if let (Some(core), Some(before)) = (remote_core, remote_dispatch_before) {
        let after = core.scheduler_statistics().actor_dispatch_count;
        assert!(
            after > before,
            "Contested round {round}: sender did not dispatch on secondary core {}",
            core.core_index()
        );
    }
    outcome
}

/// After all participating actors are gone, no frame or message charge may remain.
fn verify_race_resources(round: usize, expected_free_frame_count: usize) {
    verify_counts(0);
    let free_frame_count = with_frame_allocator(|allocator| allocator.free_frame_count())
        .unwrap_or_else(|| {
            crate::error!(
                EarlyConsole,
                "Race round {round}: frame allocator unavailable"
            );
            power_interface::halt_machine()
        });
    assert_eq!(
        free_frame_count, expected_free_frame_count,
        "Race round {round} leaked physical frames"
    );
}

/// Runs controlled orderings and true cross-core send/terminate competition.
fn run_wake_termination_races(expected_free_frame_count: usize) {
    let mut termination_wins = 0_usize;
    let mut wake_wins = 0_usize;
    let mut contested_termination_wins = 0_usize;
    let mut contested_wake_wins = 0_usize;

    information!(
        EarlyConsole,
        "Wake/terminate races starting: {RACE_ROUND_COUNT} rounds"
    );

    for round in 0..RACE_ROUND_COUNT {
        let outcome = run_race_round(round);
        match outcome {
            RaceOutcome::Termination => termination_wins += 1,
            RaceOutcome::Wake => wake_wins += 1,
        }
        if round % 3 == 2 {
            match outcome {
                RaceOutcome::Termination => contested_termination_wins += 1,
                RaceOutcome::Wake => contested_wake_wins += 1,
            }
        }
        verify_race_resources(round, expected_free_frame_count);

        if (round + 1) % RACE_REPORT_INTERVAL == 0 {
            information!(
                EarlyConsole,
                "Wake/terminate progress: {}/{} rounds, {} termination wins, {} wake wins",
                round + 1,
                RACE_ROUND_COUNT,
                termination_wins,
                wake_wins
            );
        }
    }

    information!(
        EarlyConsole,
        "Contested cross-core outcomes: {contested_termination_wins} termination, {contested_wake_wins} wake"
    );
    assert!(
        contested_termination_wins > 0 && contested_wake_wins > 0,
        "Contested cross-core races did not exercise both valid outcomes"
    );

    information!(
        EarlyConsole,
        "Wake/terminate races passed: {RACE_ROUND_COUNT} rounds, {termination_wins} termination wins, {wake_wins} wake wins (contested: {contested_termination_wins} termination, {contested_wake_wins} wake), remote dispatches verified, zero frame/message drift"
    );
}
/// Exercises a full resident population; returns only after every invariant passes.
pub fn run() {
    let available_actor_slots = system_limit().maximum_actors;
    assert!(
        available_actor_slots >= TARGET_ACTOR_COUNT,
        "Residency requires {TARGET_ACTOR_COUNT} slots, only {available_actor_slots} available"
    );

    information!(
        EarlyConsole,
        "Residency starting: target {TARGET_ACTOR_COUNT}, available {available_actor_slots} slots"
    );

    verify_counts(0);
    let baseline_free_frame_count = with_frame_allocator(|allocator| allocator.free_frame_count())
        .unwrap_or_else(|| {
            crate::error!(
                EarlyConsole,
                "Frame allocator unavailable before residency audit"
            );
            power_interface::halt_machine()
        });

    for expected_actor_count in 1..=TARGET_ACTOR_COUNT {
        if expected_actor_count <= 128 {
            information!(
                EarlyConsole,
                "Residency spawning actor {expected_actor_count}"
            );
        }

        let address = match table::enqueue(Actor::new(
            Name::UNNAMED,
            default_configuration(),
            Image::Elf(GENESIS_PROGRAM),
        )) {
            Ok(address) => address,
            Err(error) => {
                crate::error!(
                    EarlyConsole,
                    "Residency spawn failed at actor {expected_actor_count}/{TARGET_ACTOR_COUNT}: {error:?}"
                );
                power_interface::halt_machine()
            }
        };

        assign_block_forever(address);
        await_parked(address);

        if expected_actor_count % REPORT_INTERVAL == 0 {
            verify_counts(expected_actor_count);
            let free_frames = with_frame_allocator(|allocator| allocator.free_frame_count())
                .unwrap_or_else(|| {
                    crate::error!(
                        EarlyConsole,
                        "Frame allocator unavailable during residency audit"
                    );
                    power_interface::halt_machine()
                });
            information!(
                EarlyConsole,
                "Residency progress: {expected_actor_count}/{TARGET_ACTOR_COUNT} waiting, {free_frames} frames free"
            );
        }
    }

    verify_counts(TARGET_ACTOR_COUNT);
    verify_actor_table();
    information!(
        EarlyConsole,
        "Residency passed: {TARGET_ACTOR_COUNT} EL0 actors alive and waiting, zero outstanding bootstrap messages"
    );

    reclaim_resident_population(baseline_free_frame_count);
    run_wake_termination_races(baseline_free_frame_count);
    load::run(baseline_free_frame_count);
}
