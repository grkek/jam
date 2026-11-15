//! Sustained concurrent messaging, actor lifetime, and slot-reuse audit.
//!
//! Every wave starts 16 independent collectors and 256 gated EL0 senders.
//! Each collector checks the ordering and contents of messages from 16 distinct
//! senders. No sender may start until the complete population has parked.
//! All actors finish normally, after which every slot, frame and message-pool
//! allocation must return to its pre-test state before the next wave begins.
//! An additional recovery phase kills actors with retained inbox messages while
//! unrelated collectors continue processing.

mod failure;
mod reincarnation;

use core::hint::spin_loop;

use foundation::{
    actor::{Actor, Image},
    address::LocalAddress,
    name::Name,
    stress::{BOOTSTRAP_TAG, Bootstrap, LOAD_START_TAG, Scenario},
    system_call_status::SystemCallStatus,
};

use super::{GENESIS_PROGRAM, await_parked, await_recycled, verify_counts};
use crate::{
    actors::{control_block::State, divider::default_configuration, inbox::MessageMetadata, table},
    architecture::aarch64::{early_console::EarlyConsole, power_interface, processor, time},
    information,
    memory::frame_allocator::with_frame_allocator,
    messaging::delivery::tell_actor,
    scheduling::core_local::{CORE_COUNT, core_locals, current_core_local},
};

/// Independent collectors participating in every wave.
const COLLECTOR_COUNT: usize = 16;
/// Distinct senders whose order each collector verifies.
const SENDERS_PER_COLLECTOR: usize = 16;
/// Total gated senders participating simultaneously in one wave.
const SENDER_COUNT: usize = COLLECTOR_COUNT * SENDERS_PER_COLLECTOR;
/// Total actor population held resident before release.
const ACTORS_PER_WAVE: usize = COLLECTOR_COUNT + SENDER_COUNT;
/// Messages sent by each sender in a wave.
const MESSAGES_PER_SENDER: u64 = 32;
/// Full actor creation, traffic, reclamation and slot-reuse cycles.
const WAVE_COUNT: usize = 4;
/// Maximum duration of one complete traffic and reclamation phase.
const WAVE_TIMEOUT_MILLISECONDS: u64 = 30_000;

/// Fails at the first spawn error instead of silently shrinking the population.
fn spawn_load_actor(wave: usize, position: usize) -> LocalAddress {
    table::enqueue(Actor::new(
        Name::UNNAMED,
        default_configuration(),
        Image::Elf(GENESIS_PROGRAM),
    ))
    .unwrap_or_else(|error| {
        crate::error!(
            EarlyConsole,
            "Load wave {wave}: spawn of actor {position}/{ACTORS_PER_WAVE} failed: {error:?}"
        );
        power_interface::halt_machine()
    })
}

/// Sends one bootstrap and leaves its message pool charge to the EL0 receiver.
fn assign_load_scenario(address: LocalAddress, peer: LocalAddress, scenario: Scenario, role: u64) {
    let bootstrap = Bootstrap {
        peer: peer.to_packed(),
        role,
        scenario,
        seed: 0,
        param: MESSAGES_PER_SENDER,
    }
    .encode();
    let metadata = MessageMetadata {
        sender: address,
        tag: BOOTSTRAP_TAG,
        correlation: 0,
    };
    if let Err(error) = tell_actor(address, metadata, &bootstrap) {
        crate::error!(
            EarlyConsole,
            "Load bootstrap of {address} ({scenario:?}) failed: {error:?}"
        );
        power_interface::halt_machine();
    }
}

/// A recycled slot must be reused only with its next generation.
fn verify_reused_generation(
    wave: usize,
    position: usize,
    address: LocalAddress,
    preceding: Option<LocalAddress>,
) {
    if let Some(previous) = preceding {
        assert_eq!(
            address.slot_index(),
            previous.slot_index(),
            "Load wave {wave} changed slot assignment at position {position}"
        );
        assert_eq!(
            address.generation_counter(),
            previous.generation_counter().wrapping_add(1),
            "Load wave {wave} did not advance generation at position {position}"
        );
    }
}

/// Builds all collectors and gated senders, confirming every actor parked.
fn start_parked_population(
    wave: usize,
    preceding: &[Option<LocalAddress>; ACTORS_PER_WAVE],
    sender_scenario: Scenario,
) -> [Option<LocalAddress>; ACTORS_PER_WAVE] {
    let mut actors = [None; ACTORS_PER_WAVE];
    let (collectors, senders) = actors.split_at_mut(COLLECTOR_COUNT);

    for (collector_index, entry) in collectors.iter_mut().enumerate() {
        let address = spawn_load_actor(wave, collector_index);
        verify_reused_generation(
            wave,
            collector_index,
            address,
            preceding.get(collector_index).copied().flatten(),
        );
        assign_load_scenario(
            address,
            address,
            Scenario::Collector,
            SENDERS_PER_COLLECTOR as u64,
        );
        await_parked(address);
        *entry = Some(address);
    }

    for (sender_index, entry) in senders.iter_mut().enumerate() {
        let position = COLLECTOR_COUNT + sender_index;
        let collector_index = sender_index / SENDERS_PER_COLLECTOR;
        let collector = collectors
            .get(collector_index)
            .copied()
            .flatten()
            .unwrap_or_else(|| power_interface::halt_machine());
        let address = spawn_load_actor(wave, position);
        verify_reused_generation(
            wave,
            position,
            address,
            preceding.get(position).copied().flatten(),
        );
        assign_load_scenario(
            address,
            collector,
            sender_scenario,
            (sender_index % SENDERS_PER_COLLECTOR) as u64,
        );
        await_parked(address);
        *entry = Some(address);
    }

    // No sender has been released yet: every bootstrap has been consumed,
    // all inboxes are empty, and all 272 actors remain simultaneously alive.
    verify_counts(ACTORS_PER_WAVE);
    information!(
        EarlyConsole,
        "Load wave {wave}/{WAVE_COUNT}: {ACTORS_PER_WAVE} EL0 actors parked, releasing {SENDER_COUNT} senders"
    );
    actors
}

/// Releases a gated sender. Only temporary shared-pool exhaustion may be retried.
fn release_sender(address: LocalAddress) {
    let deadline = time::read_counter_value().saturating_add(
        time::read_counter_frequency().saturating_mul(WAVE_TIMEOUT_MILLISECONDS) / 1_000,
    );
    let metadata = MessageMetadata {
        sender: address,
        tag: LOAD_START_TAG,
        correlation: 0,
    };
    loop {
        match tell_actor(address, metadata, &[]) {
            Ok(()) => return,
            Err(SystemCallStatus::BudgetExhausted) => {
                assert!(
                    time::read_counter_value() < deadline,
                    "Load sender {address}: shared message pool remained exhausted"
                );
                spin_loop();
            }
            Err(error) => {
                crate::error!(EarlyConsole, "Load start of {address} failed: {error:?}");
                power_interface::halt_machine();
            }
        }
    }
}

/// Waits for an exact actor incarnation to finish its owned cleanup.
fn await_load_recycled(address: LocalAddress, deadline: u64) {
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
            // Check stale-address rejection using the residency audit helper.
            await_recycled(address);
            return;
        }
        assert!(
            time::read_counter_value() < deadline,
            "Load actor {address} was not reclaimed before the wave deadline"
        );
        processor::wait_for_event();
    }
}

/// Fails if an actor was left behind or any physical/message accounting drifted.
fn verify_wave_resources(wave: usize, baseline_free_frame_count: usize) {
    verify_counts(0);
    let free_frame_count = with_frame_allocator(|allocator| allocator.free_frame_count())
        .unwrap_or_else(|| power_interface::halt_machine());
    assert_eq!(
        free_frame_count, baseline_free_frame_count,
        "Load wave {wave} leaked physical frames"
    );
}

/// Verifies that the load dispatched onto at least one secondary core.
fn verify_secondary_dispatch(dispatch_before: &[u64; CORE_COUNT]) {
    let boot_core_index = current_core_local()
        .unwrap_or_else(|| power_interface::halt_machine())
        .core_index();
    let secondary_dispatch_count = core_locals()
        .iter()
        .zip(dispatch_before)
        .filter(|(core, before)| {
            core.is_online()
                && core.core_index() != boot_core_index
                && core.scheduler_statistics().actor_dispatch_count > **before
        })
        .count();
    assert!(
        secondary_dispatch_count > 0,
        "Sustained load did not dispatch on a secondary core"
    );
}

/// Runs every actor of a wave concurrently and waits for complete reclamation.
fn execute_wave(actors: &[Option<LocalAddress>; ACTORS_PER_WAVE]) {
    let dispatch_before = core::array::from_fn(|index| {
        core_locals()
            .get(index)
            .map_or(0, |core| core.scheduler_statistics().actor_dispatch_count)
    });

    // Steer parked senders across secondary cores before any start signal.
    // The scheduler remains free to override these hints when balancing load.
    for (index, address) in actors.iter().skip(COLLECTOR_COUNT).flatten().enumerate() {
        let secondary = super::secondary_core_for_round(index * 3 + 2);
        let _ = super::prepare_remote_sender(*address, secondary);
    }

    for address in actors.iter().skip(COLLECTOR_COUNT).flatten() {
        release_sender(*address);
    }

    let deadline = time::read_counter_value().saturating_add(
        time::read_counter_frequency().saturating_mul(WAVE_TIMEOUT_MILLISECONDS) / 1_000,
    );
    for address in actors.iter().flatten() {
        await_load_recycled(*address, deadline);
    }
    verify_secondary_dispatch(&dispatch_before);
}

/// Runs three independent bursts of validated concurrent messages and checks slot reuse.
pub(super) fn run(baseline_free_frame_count: usize) {
    let mut previous = [None; ACTORS_PER_WAVE];
    information!(
        EarlyConsole,
        "Sustained load starting: {WAVE_COUNT} waves, {ACTORS_PER_WAVE} actors per wave, {} messages total",
        WAVE_COUNT * SENDER_COUNT * MESSAGES_PER_SENDER as usize
    );

    for wave in 1..=WAVE_COUNT {
        let actors = start_parked_population(wave, &previous, Scenario::LoadSender);
        execute_wave(&actors);
        verify_wave_resources(wave, baseline_free_frame_count);
        previous = actors;
        information!(
            EarlyConsole,
            "Load wave {wave}/{WAVE_COUNT} passed: {ACTORS_PER_WAVE} recycled, {} messages verified, zero frame/message drift",
            SENDER_COUNT * MESSAGES_PER_SENDER as usize
        );
    }
    information!(
        EarlyConsole,
        "Sustained load passed: {WAVE_COUNT} waves, {} actor lifetimes, {} validated messages, zero frame/message drift",
        WAVE_COUNT * ACTORS_PER_WAVE,
        WAVE_COUNT * SENDER_COUNT * MESSAGES_PER_SENDER as usize
    );

    failure::run(baseline_free_frame_count, &previous);
    reincarnation::run(baseline_free_frame_count);
}
