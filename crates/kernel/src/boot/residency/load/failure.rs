//! Failure recovery while independent EL0 actors exchange validated messages.
//!
//! Two waves each start 272 healthy collector/sender actors plus 16 parked
//! victims and 16 EL0 probes. Victims retain unmatched messages. A probe first
//! delivers to its live victim, then parks; the kernel terminates that victim
//! while the healthy workload remains resident. The same probe later verifies
//! that the stale address rejects every attempted delivery.

use core::hint::spin_loop;

use foundation::{
    address::LocalAddress,
    stress::{HAMMER_TAG, RECOVERY_VERIFY_TAG, Scenario},
    system_call_status::SystemCallStatus,
};

use super::{
    ACTORS_PER_WAVE, COLLECTOR_COUNT, MESSAGES_PER_SENDER, SENDER_COUNT, WAVE_COUNT,
    WAVE_TIMEOUT_MILLISECONDS, assign_load_scenario, await_load_recycled, release_sender,
    spawn_load_actor, start_parked_population, verify_reused_generation, verify_secondary_dispatch,
    verify_wave_resources,
};
use crate::{
    actors::{
        control_block::State, inbox::MessageMetadata, lifecycle::terminate_waiting_actor,
        message_storage::MESSAGE_POOL, snapshot, table,
    },
    architecture::aarch64::{early_console::EarlyConsole, power_interface, processor, time},
    information,
    messaging::delivery::tell_actor,
    scheduling::core_local::{CORE_COUNT, core_locals},
};

/// Terminated actors that each retain several queued messages.
const VICTIM_COUNT: usize = 16;
/// Unmatched kernel-sent messages queued before EL0 probes run.
const PREFILLED_MESSAGES_PER_VICTIM: usize = 2;
/// Queued messages per victim after its EL0 probe has delivered one more.
const MESSAGES_PER_VICTIM: usize = PREFILLED_MESSAGES_PER_VICTIM + 1;
/// Complete load/failure/reclamation cycles.
const RECOVERY_WAVE_COUNT: usize = 2;
/// Actors alive while the recovery population is parked.
const TOTAL_ACTOR_COUNT: usize = ACTORS_PER_WAVE + 2 * VICTIM_COUNT;

/// One complete population, with stable addresses retained for generation checks.
struct Population {
    healthy: [Option<LocalAddress>; ACTORS_PER_WAVE],
    victims: [Option<LocalAddress>; VICTIM_COUNT],
    probes: [Option<LocalAddress>; VICTIM_COUNT],
}

/// Fails rather than silently proceeding when the requested address is absent.
#[must_use]
fn address(entry: Option<LocalAddress>) -> LocalAddress {
    entry.unwrap_or_else(|| power_interface::halt_machine())
}

/// Waits until one victim's inbox holds the requested number of unmatched messages.
fn await_queued_messages(victim: LocalAddress, expected: usize) {
    let deadline = time::read_counter_value().saturating_add(
        time::read_counter_frequency().saturating_mul(WAVE_TIMEOUT_MILLISECONDS) / 1_000,
    );
    loop {
        let count = table::with_control_block(victim.slot_index() as usize, |actor| {
            assert_eq!(actor.generation_counter, victim.generation_counter());
            assert_eq!(
                actor.state,
                State::Waiting,
                "Victim {victim} woke unexpectedly"
            );
            assert!(!actor.queued, "Victim {victim} has a runnable queue entry");
            actor.inbox.as_ref().map(|inbox| inbox.message_count())
        })
        .flatten();
        if count == Some(expected) {
            return;
        }
        assert!(
            time::read_counter_value() < deadline,
            "Victim {victim} did not retain {expected} messages (observed {count:?})"
        );
        processor::wait_for_event();
    }
}

/// Delivers deliberately unmatched messages which must be drained on termination.
fn prefill_victim(victim: LocalAddress) {
    for sequence in 0..PREFILLED_MESSAGES_PER_VICTIM {
        let metadata = MessageMetadata {
            sender: victim,
            tag: HAMMER_TAG,
            correlation: sequence as u64,
        };
        assert_eq!(
            tell_actor(victim, metadata, &[0xa5; 32]),
            Ok(()),
            "Could not queue unmatched message for victim {victim}"
        );
    }
    await_queued_messages(victim, PREFILLED_MESSAGES_PER_VICTIM);
}

/// Spawns victims and probes behind the still-parked healthy population.
#[must_use]
fn build_population(
    wave: usize,
    previous: Option<&Population>,
    preceding_healthy: &[Option<LocalAddress>; ACTORS_PER_WAVE],
) -> Population {
    let healthy =
        start_parked_population(WAVE_COUNT + wave, preceding_healthy, Scenario::LoadSender);
    let mut victims = [None; VICTIM_COUNT];
    let mut probes = [None; VICTIM_COUNT];

    for (index, entry) in victims.iter_mut().enumerate() {
        let victim = spawn_load_actor(wave, ACTORS_PER_WAVE + index);
        verify_reused_generation(
            wave,
            ACTORS_PER_WAVE + index,
            victim,
            previous.and_then(|population| population.victims.get(index).copied().flatten()),
        );
        super::super::assign_block_forever(victim);
        super::super::await_parked(victim);
        prefill_victim(victim);
        *entry = Some(victim);
    }

    for (index, entry) in probes.iter_mut().enumerate() {
        let victim = address(victims.get(index).copied().flatten());
        let probe = spawn_load_actor(wave, ACTORS_PER_WAVE + VICTIM_COUNT + index);
        verify_reused_generation(
            wave,
            ACTORS_PER_WAVE + VICTIM_COUNT + index,
            probe,
            previous.and_then(|population| population.probes.get(index).copied().flatten()),
        );
        assign_load_scenario(probe, victim, Scenario::RecoveryProbe, 0);
        super::super::await_parked(probe);
        *entry = Some(probe);
    }

    let (occupied, waiting) = snapshot::current();
    assert_eq!((occupied, waiting), (TOTAL_ACTOR_COUNT, TOTAL_ACTOR_COUNT));
    assert_eq!(
        MESSAGE_POOL.lock().statistics().allocated,
        VICTIM_COUNT * PREFILLED_MESSAGES_PER_VICTIM,
        "Unexpected message pool allocations before releasing recovery probes"
    );
    Population {
        healthy,
        victims,
        probes,
    }
}

/// Gives each EL0 probe a chance to send one message to its *live* victim.
fn deliver_before_failure(population: &Population) {
    for (index, probe) in population.probes.iter().flatten().enumerate() {
        let remote = super::super::secondary_core_for_round(index);
        let _ = super::super::prepare_remote_sender(*probe, remote);
        release_sender(*probe);
    }
    for (victim, probe) in population.victims.iter().zip(&population.probes) {
        await_queued_messages(address(*victim), MESSAGES_PER_VICTIM);
        super::super::await_parked(address(*probe));
    }
    let (occupied, waiting) = snapshot::current();
    assert_eq!((occupied, waiting), (TOTAL_ACTOR_COUNT, TOTAL_ACTOR_COUNT));
    assert_eq!(
        MESSAGE_POOL.lock().statistics().allocated,
        VICTIM_COUNT * MESSAGES_PER_VICTIM,
        "Recovery probes did not leave their live-target messages queued"
    );
}

/// Releases half the healthy senders, keeping the other half and all collectors alive.
#[must_use]
fn release_first_healthy_half(population: &Population) -> [u64; CORE_COUNT] {
    let before = core::array::from_fn(|index| {
        core_locals()
            .get(index)
            .map_or(0, |core| core.scheduler_statistics().actor_dispatch_count)
    });
    for (index, sender) in population
        .healthy
        .iter()
        .skip(COLLECTOR_COUNT)
        .flatten()
        .enumerate()
    {
        let remote = super::super::secondary_core_for_round(index * 3 + 2);
        let _ = super::super::prepare_remote_sender(*sender, remote);
    }
    for (index, sender) in population
        .healthy
        .iter()
        .skip(COLLECTOR_COUNT)
        .flatten()
        .enumerate()
    {
        if index % 2 == 0 {
            release_sender(*sender);
        }
    }
    before
}

/// Reclaims victims while unrelated collectors cannot yet have completed.
fn terminate_victims(population: &Population) {
    let (occupied, _) = snapshot::current();
    assert!(
        occupied >= 2 * VICTIM_COUNT + COLLECTOR_COUNT + SENDER_COUNT / 2,
        "Healthy workload finished before failure injection began"
    );
    for victim in population.victims.iter().flatten() {
        await_queued_messages(*victim, MESSAGES_PER_VICTIM);
        assert_eq!(
            terminate_waiting_actor(*victim),
            Ok(()),
            "Could not reclaim parked victim {victim} with queued messages"
        );
        let metadata = MessageMetadata {
            sender: *victim,
            tag: HAMMER_TAG,
            correlation: 0,
        };
        assert_eq!(
            tell_actor(*victim, metadata, b"stale"),
            Err(SystemCallStatus::MalformedAddress),
            "Terminated victim {victim} accepted a stale message"
        );
        super::super::await_recycled(*victim);
    }
}

/// Retries one confirmation signal under concurrent traffic and bounded pool pressure.
fn release_probe_confirmation(probe: LocalAddress) {
    let deadline = time::read_counter_value().saturating_add(
        time::read_counter_frequency().saturating_mul(WAVE_TIMEOUT_MILLISECONDS) / 1_000,
    );
    let metadata = MessageMetadata {
        sender: probe,
        tag: RECOVERY_VERIFY_TAG,
        correlation: 0,
    };
    loop {
        match tell_actor(probe, metadata, &[]) {
            Ok(()) => return,
            Err(SystemCallStatus::BudgetExhausted) => {
                assert!(
                    time::read_counter_value() < deadline,
                    "Recovery confirmation to probe {probe} exhausted the message pool"
                );
                spin_loop();
            }
            Err(error) => {
                crate::error!(EarlyConsole, "Recovery confirmation to {probe}: {error:?}");
                power_interface::halt_machine();
            }
        }
    }
}

/// Completes the healthy traffic and verifies EL0 stale-address checks and reclaim.
fn finish_population(population: &Population, dispatch_before: &[u64; CORE_COUNT]) {
    for (index, sender) in population
        .healthy
        .iter()
        .skip(COLLECTOR_COUNT)
        .flatten()
        .enumerate()
    {
        if index % 2 != 0 {
            release_sender(*sender);
        }
    }
    for probe in population.probes.iter().flatten() {
        release_probe_confirmation(*probe);
    }
    let deadline = time::read_counter_value().saturating_add(
        time::read_counter_frequency().saturating_mul(WAVE_TIMEOUT_MILLISECONDS) / 1_000,
    );
    for actor in population
        .healthy
        .iter()
        .chain(&population.probes)
        .flatten()
    {
        await_load_recycled(*actor, deadline);
    }
    verify_secondary_dispatch(dispatch_before);
}

/// Runs two full failure/recovery waves after the ordinary sustained-load audit.
pub(super) fn run(
    baseline_free_frame_count: usize,
    preceding_healthy: &[Option<LocalAddress>; ACTORS_PER_WAVE],
) {
    let mut previous: Option<Population> = None;
    let mut previous_healthy = *preceding_healthy;
    information!(
        EarlyConsole,
        "Failure recovery starting: {RECOVERY_WAVE_COUNT} waves, {VICTIM_COUNT} victims per wave, {} healthy messages",
        RECOVERY_WAVE_COUNT * SENDER_COUNT * MESSAGES_PER_SENDER as usize
    );

    for wave in 1..=RECOVERY_WAVE_COUNT {
        let population = build_population(wave, previous.as_ref(), &previous_healthy);
        deliver_before_failure(&population);
        let dispatch_before = release_first_healthy_half(&population);
        terminate_victims(&population);
        finish_population(&population, &dispatch_before);
        verify_wave_resources(WAVE_COUNT + wave, baseline_free_frame_count);
        previous_healthy = population.healthy;
        previous = Some(population);
        information!(
            EarlyConsole,
            "Failure recovery wave {wave}/{RECOVERY_WAVE_COUNT} passed: {VICTIM_COUNT} victims reclaimed, {VICTIM_COUNT} stale-address probes, {} healthy messages validated, zero resource drift",
            SENDER_COUNT * MESSAGES_PER_SENDER as usize
        );
    }
    information!(
        EarlyConsole,
        "Failure recovery passed: {} queued-message victims, {} EL0 stale-address probes, {} healthy messages, zero frame/message drift",
        RECOVERY_WAVE_COUNT * VICTIM_COUNT,
        RECOVERY_WAVE_COUNT * VICTIM_COUNT,
        RECOVERY_WAVE_COUNT * SENDER_COUNT * MESSAGES_PER_SENDER as usize
    );
}
