//! Generation-safe actor reincarnation under simultaneously validated EL0 traffic.
//!
//! A resident background population keeps its slots occupied while one target
//! slot is repeatedly recycled. Multiple EL0 senders attempt stale deliveries
//! to older incarnations and concurrently deliver an independently validated
//! FIFO stream to the current collector. The kernel checks every historical
//! address against each new incarnation, followed by complete resource audit.

use core::hint::spin_loop;

use foundation::{
    address::LocalAddress,
    name::Name,
    stress::{
        BOOTSTRAP_TAG, Bootstrap, LOAD_START_TAG, REINCARNATION_STALE_SENDS_PER_MESSAGE,
        REINCARNATION_STOP_TAG, Scenario,
    },
    system_call_status::SystemCallStatus,
    tag::Tag,
};

use super::{
    ACTORS_PER_WAVE, COLLECTOR_COUNT, MESSAGES_PER_SENDER, SENDER_COUNT, SENDERS_PER_COLLECTOR,
    WAVE_TIMEOUT_MILLISECONDS, await_load_recycled, release_sender, spawn_load_actor,
    start_parked_population, verify_secondary_dispatch, verify_wave_resources,
};
use crate::{
    actors::{
        control_block::State, inbox::MessageMetadata, lifecycle::terminate_waiting_actor, snapshot,
        table,
    },
    architecture::aarch64::{early_console::EarlyConsole, power_interface, time},
    information,
    memory::frame_allocator::with_frame_allocator,
    messaging::delivery::tell_actor,
    scheduling::core_local::{CORE_COUNT, core_locals},
};

/// Successive generations of one slot validated while other actors remain live.
const REINCARNATION_CYCLES: usize = 32;
/// Independent EL0 probes contending on each fresh collector.
const PROBES_PER_CYCLE: usize = 8;
/// Fresh messages validated from each probe, in strict per-sender FIFO order.
const MESSAGES_PER_PROBE: u64 = 16;
/// One initial parked victim plus every completed collector incarnation.
const HISTORY_CAPACITY: usize = REINCARNATION_CYCLES + 1;
/// Expected newly validated messages delivered to successive collectors.
const TOTAL_FRESH_MESSAGES: usize =
    REINCARNATION_CYCLES * PROBES_PER_CYCLE * MESSAGES_PER_PROBE as usize;
/// Rejected EL0 stale-address messages, interleaved with accepted messages.
const TOTAL_STALE_ATTEMPTS: usize =
    TOTAL_FRESH_MESSAGES * REINCARNATION_STALE_SENDS_PER_MESSAGE as usize;

/// Returns the initialised entry or halts the test on corrupt population state.
fn require_address(address: Option<LocalAddress>) -> LocalAddress {
    address.unwrap_or_else(|| power_interface::halt_machine())
}

/// Enqueues a fresh actor in the lowest free slot.
fn spawn_reincarnation_actor(cycle: usize, position: usize) -> LocalAddress {
    spawn_load_actor(cycle, ACTORS_PER_WAVE + position)
}

/// Bootstraps one actor with a new collector address and optional old address.
fn assign_reincarnation_scenario(
    actor: LocalAddress,
    peer: LocalAddress,
    scenario: Scenario,
    role: u64,
    seed: u64,
    messages: u64,
) {
    let bootstrap = Bootstrap {
        peer: peer.to_packed(),
        role,
        scenario,
        seed,
        param: messages,
    }
    .encode();
    let metadata = MessageMetadata {
        sender: actor,
        tag: BOOTSTRAP_TAG,
        correlation: 0,
    };
    assert_eq!(
        tell_actor(actor, metadata, &bootstrap),
        Ok(()),
        "Reincarnation bootstrap rejected for {actor}"
    );
}

/// Sends a notification with a bounded wait for pool capacity.
fn notify_actor(actor: LocalAddress, tag: Tag) {
    let deadline = deadline();
    let metadata = MessageMetadata {
        sender: actor,
        tag,
        correlation: 0,
    };
    loop {
        match tell_actor(actor, metadata, &[]) {
            Ok(()) => return,
            Err(SystemCallStatus::BudgetExhausted) => {
                assert!(
                    time::read_counter_value() < deadline,
                    "Notification to {actor} timed out"
                );
                spin_loop();
            }
            Err(error) => panic!("Notification to {actor} failed: {error:?}"),
        }
    }
}

/// Current counter deadline for a bounded round.
fn deadline() -> u64 {
    time::read_counter_value().saturating_add(
        time::read_counter_frequency().saturating_mul(WAVE_TIMEOUT_MILLISECONDS) / 1_000,
    )
}

/// Checks that one target actor is parked, has no messages and owns its slot.
fn verify_empty_waiting_actor(address: LocalAddress) {
    let valid = table::with_control_block(address.slot_index() as usize, |actor| {
        actor.generation_counter == address.generation_counter()
            && actor.state == State::Waiting
            && !actor.queued
            && actor
                .inbox
                .as_ref()
                .is_some_and(|inbox| inbox.message_count() == 0)
    });
    assert_eq!(
        valid,
        Some(true),
        "Fresh collector {address} is not empty and parked"
    );
}

/// Starts a victim with unmatched queued data, which must be freed on termination.
fn terminate_initial_victim() -> LocalAddress {
    let victim = spawn_reincarnation_actor(0, 0);
    super::super::assign_block_forever(victim);
    super::super::await_parked(victim);

    for correlation in 0..2_u64 {
        let metadata = MessageMetadata {
            sender: victim,
            tag: LOAD_START_TAG,
            correlation,
        };
        assert_eq!(
            tell_actor(victim, metadata, b"retained victim payload"),
            Ok(())
        );
    }
    let queued = table::with_control_block(victim.slot_index() as usize, |actor| {
        actor.state == State::Waiting
            && actor
                .inbox
                .as_ref()
                .is_some_and(|inbox| inbox.message_count() == 2)
    });
    assert_eq!(
        queued,
        Some(true),
        "Initial victim {victim} did not retain two messages"
    );
    assert_eq!(terminate_waiting_actor(victim), Ok(()));
    super::super::await_recycled(victim);
    victim
}

/// Proves that *every* historical address is rejected while the new actor lives.
fn verify_historical_addresses(
    history: &[Option<LocalAddress>; HISTORY_CAPACITY],
    history_length: usize,
    fresh: LocalAddress,
) {
    verify_empty_waiting_actor(fresh);
    for old in history[..history_length].iter().copied() {
        let old = require_address(old);
        assert_eq!(old.slot_index(), fresh.slot_index());
        assert_ne!(old.generation_counter(), fresh.generation_counter());
        let metadata = MessageMetadata {
            sender: old,
            tag: LOAD_START_TAG,
            correlation: history_length as u64,
        };
        assert_eq!(
            tell_actor(old, metadata, b"must never reach new collector"),
            Err(SystemCallStatus::MalformedAddress),
            "Stale incarnation {old} was accepted while {fresh} occupied the slot"
        );
    }
    verify_empty_waiting_actor(fresh);
}

/// Creates all probes before releasing any, ensuring simultaneous residency.
fn prepare_probes(
    cycle: usize,
    history: &[Option<LocalAddress>; HISTORY_CAPACITY],
    fresh: LocalAddress,
) -> [Option<LocalAddress>; PROBES_PER_CYCLE] {
    let mut probes = [None; PROBES_PER_CYCLE];
    for (index, entry) in probes.iter_mut().enumerate() {
        let probe = spawn_reincarnation_actor(cycle, index + 1);
        // Probe zero always targets the immediate predecessor; remaining probes
        // rotate through older generations, including the original parked victim.
        let history_index = if index == 0 {
            cycle - 1
        } else {
            (cycle + index) % cycle
        };
        let old = require_address(history[history_index]);
        assign_reincarnation_scenario(
            probe,
            fresh,
            Scenario::ReincarnationProbe,
            index as u64,
            old.to_packed(),
            MESSAGES_PER_PROBE,
        );
        super::super::await_parked(probe);
        *entry = Some(probe);
    }
    probes
}

/// Releases one subset of the healthy senders while collector lanes remain live.
fn release_background_traffic(cycle: usize, healthy: &[Option<LocalAddress>; ACTORS_PER_WAVE]) {
    // Never start the final sender in each collector's group yet. Every
    // collector therefore remains resident for the entire reincarnation phase.
    for (sender_index, sender) in healthy.iter().skip(COLLECTOR_COUNT).enumerate() {
        if sender_index % REINCARNATION_CYCLES != cycle - 1
            || sender_index % SENDERS_PER_COLLECTOR == SENDERS_PER_COLLECTOR - 1
        {
            continue;
        }
        let sender = require_address(*sender);
        let remote = super::super::secondary_core_for_round(sender_index * 3 + 2);
        let _ = super::super::prepare_remote_sender(sender, remote);
        release_sender(sender);
    }
}

/// Runs one complete slot incarnation with concurrent live and stale messages.
fn run_cycle(
    cycle: usize,
    previous: LocalAddress,
    history: &[Option<LocalAddress>; HISTORY_CAPACITY],
    healthy: &[Option<LocalAddress>; ACTORS_PER_WAVE],
    resident_free_frames: usize,
) -> LocalAddress {
    let collector = spawn_reincarnation_actor(cycle, 0);
    assert_eq!(
        collector.slot_index(),
        previous.slot_index(),
        "Target slot moved in cycle {cycle}"
    );
    assert_eq!(
        collector.generation_counter(),
        previous.generation_counter().wrapping_add(1),
        "Target slot generation was not advanced in cycle {cycle}"
    );
    assign_reincarnation_scenario(
        collector,
        collector,
        Scenario::Collector,
        PROBES_PER_CYCLE as u64,
        0,
        MESSAGES_PER_PROBE,
    );
    super::super::await_parked(collector);
    verify_historical_addresses(history, cycle, collector);

    let probes = prepare_probes(cycle, history, collector);
    let dispatch_before: [u64; CORE_COUNT] = core::array::from_fn(|index| {
        core_locals()
            .get(index)
            .map_or(0, |core| core.scheduler_statistics().actor_dispatch_count)
    });

    for (index, probe) in probes.iter().enumerate() {
        let probe = require_address(*probe);
        let remote = super::super::secondary_core_for_round(index * 3 + 2);
        let _ = super::super::prepare_remote_sender(probe, remote);
    }
    for probe in probes.iter().copied() {
        release_sender(require_address(probe));
    }
    release_background_traffic(cycle, healthy);

    let round_deadline = deadline();
    for actor in probes
        .iter()
        .copied()
        .chain(core::iter::once(Some(collector)))
    {
        await_load_recycled(require_address(actor), round_deadline);
    }
    verify_secondary_dispatch(&dispatch_before);

    // No healthy actor can exit before its explicit shutdown: one sender per
    // collector is still parked, and previously released senders await STOP.
    let (occupied, _) = snapshot::current();
    assert_eq!(
        occupied, ACTORS_PER_WAVE,
        "Healthy actor population changed in cycle {cycle}"
    );
    let free_frames = with_frame_allocator(|allocator| allocator.free_frame_count())
        .unwrap_or_else(|| power_interface::halt_machine());
    assert_eq!(
        free_frames, resident_free_frames,
        "Frame drift after reincarnation cycle {cycle}"
    );
    collector
}

/// Releases the withheld sender in each lane and then lets resident senders exit.
fn finish_healthy_population(healthy: &[Option<LocalAddress>; ACTORS_PER_WAVE]) {
    for (index, sender) in healthy.iter().skip(COLLECTOR_COUNT).enumerate() {
        if index % SENDERS_PER_COLLECTOR == SENDERS_PER_COLLECTOR - 1 {
            release_sender(require_address(*sender));
        }
    }
    // STOP messages may be queued before a sender finishes its workload; they
    // remain in its inbox until it waits for the shutdown tag.
    for sender in healthy.iter().skip(COLLECTOR_COUNT).copied() {
        notify_actor(require_address(sender), REINCARNATION_STOP_TAG);
    }
    let finish_deadline = deadline();
    for actor in healthy.iter().copied() {
        await_load_recycled(require_address(actor), finish_deadline);
    }
}

/// Verifies no table slot retains actor-owned state after all test phases.
fn verify_final_actor_table() {
    for slot_index in 0..table::actor_table_length() {
        let clean = table::with_control_block(slot_index, |actor| {
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
        });
        assert_eq!(
            clean,
            Some(true),
            "Actor table retained state in slot {slot_index}"
        );
    }
}

/// Audits repeated generations, old-address isolation, payload integrity and cleanup.
pub(super) fn run(baseline_free_frame_count: usize) {
    information!(
        EarlyConsole,
        "Reincarnation starting: {REINCARNATION_CYCLES} generations, {} EL0 stale attempts, {TOTAL_FRESH_MESSAGES} checked fresh messages",
        TOTAL_STALE_ATTEMPTS
    );
    let healthy = start_parked_population(
        10,
        &[None; ACTORS_PER_WAVE],
        Scenario::ReincarnationTrafficSender,
    );
    let resident_free_frames = with_frame_allocator(|allocator| allocator.free_frame_count())
        .unwrap_or_else(|| power_interface::halt_machine());

    let mut history = [None; HISTORY_CAPACITY];
    let mut previous = terminate_initial_victim();
    history[0] = Some(previous);
    let victim_free_frames = with_frame_allocator(|allocator| allocator.free_frame_count())
        .unwrap_or_else(|| power_interface::halt_machine());
    assert_eq!(
        resident_free_frames, victim_free_frames,
        "Initial victim cleanup leaked frames"
    );

    for cycle in 1..=REINCARNATION_CYCLES {
        let current = run_cycle(cycle, previous, &history, &healthy, resident_free_frames);
        history[cycle] = Some(current);
        previous = current;
        if cycle % 8 == 0 {
            information!(
                EarlyConsole,
                "Reincarnation progress: {cycle}/{REINCARNATION_CYCLES} generations, {} stale EL0 sends rejected, {} fresh messages validated",
                cycle
                    * PROBES_PER_CYCLE
                    * MESSAGES_PER_PROBE as usize
                    * REINCARNATION_STALE_SENDS_PER_MESSAGE as usize,
                cycle * PROBES_PER_CYCLE * MESSAGES_PER_PROBE as usize,
            );
        }
    }

    finish_healthy_population(&healthy);
    verify_wave_resources(10, baseline_free_frame_count);
    verify_final_actor_table();
    information!(
        EarlyConsole,
        "Local actor lifecycle suite passed: {REINCARNATION_CYCLES} reincarnations, {TOTAL_STALE_ATTEMPTS} EL0 stale sends rejected, {TOTAL_FRESH_MESSAGES} fresh messages and {} background messages validated, zero frame/message drift, all actor slots clear",
        SENDER_COUNT * MESSAGES_PER_SENDER as usize,
    );
}
