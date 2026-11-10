//! Boot-time messaging stress plan.
//!
//! Launches many instances of the genesis program, each bootstrapped with a
//! `Scenario`. Phases run in an order that keeps the plan deterministic:
//! everything that needs a healthy message pool runs first, and the phase that
//! deliberately exhausts it runs last.
//!
//! Bootstrap messages are sent to a receiver before anything that floods it, and
//! transient refusals are retried, so a refusal that survives is a real finding.

use core::hint::spin_loop;

use foundation::{
    address::LocalAddress,
    stress::{BOOTSTRAP_TAG, Bootstrap, HAMMER_TAG, Scenario},
};

use crate::{
    actors::{divider::default_configuration, inbox::MessageMetadata, table::spawn_user_actor},
    architecture::aarch64::{early_console::EarlyConsole, processor, time::read_counter_value},
    information,
    memory::user_program::EL0_GENESIS_PROGRAM,
    messaging::delivery::tell_actor,
    system_calls::actor_calls::pack_local_address,
    warning,
};

const COLLECTOR_SENDERS: usize = 6;
const COLLECTOR_MESSAGES_PER_SENDER: u64 = 400;
const CLIENTS: usize = 4;
const CRASHING_CLIENTS: usize = 2;
const REQUESTS_PER_CLIENT: u64 = 200;
const PING_PONG_PAIRS: [u64; 3] = [0, 0, 40]; // die-after sequence; 0 = healthy
const SOLO_VALIDATORS: [Scenario; 6] = [
    Scenario::SelfCycle,
    Scenario::SelfCycle,
    Scenario::SizeProbe,
    Scenario::SizeProbe,
    Scenario::SystemCallFuzz,
    Scenario::SystemCallFuzz,
];
const FAULTS: [Scenario; 7] = [
    Scenario::CrashBreakpoint,
    Scenario::CrashNullWrite,
    Scenario::CrashExecuteData,
    Scenario::CrashUndefined,
    Scenario::CrashPanic,
    Scenario::CrashStackOverflow,
    Scenario::CrashWriteCode,
];
const CPU_HOGS: usize = 2;
const DEAD_LETTER_SPAMMERS: usize = 2;

const CHURN_WAVES: usize = 8;
const CHURN_WAVE_SIZE: usize = 32;
const CHURN_INTERVAL_MILLISECONDS: u64 = 500;
const DEAD_HAMMER_MESSAGES: usize = 256;

const QUIET_PERIOD_MILLISECONDS: u64 = 500;
const STRICT: u64 = 1;

const ORPHANS: usize = 2;
const FILLERS_PER_ORPHAN: usize = 2;
const IDLE_HAMMER_MESSAGES: usize = 4096;

const BOOTSTRAP_ATTEMPTS: u32 = 100_000;
const FALLBACK_COUNTER_FREQUENCY: u64 = 62_500_000;

/// Runs the whole plan. Returns once every phase has been launched and reported.
pub fn run() {
    information!(EarlyConsole, "Stress plan: beginning actor creation");

    let mut plan = Plan::new();
    plan.request_reply_pairs();
    plan.collector_flood();
    plan.server_with_clients();
    plan.solo_validators();
    plan.fault_injection();
    plan.cpu_hogs();
    plan.dead_letter_spam();
    plan.churn_waves();
    plan.quiet_leak_check();
    plan.report("Stress core complete");

    plan.exhaustion();
    plan.report("Stress done");
}

struct Plan {
    /// Random seed for bootstrap messages, used to generate payloads and
    seed: u64,
    /// Number of actors successfully spawned.
    spawned: usize,
    /// Number of actors that failed to spawn (pool pressure, etc.).
    spawn_failures: usize,
    /// Actors that were spawned but never received a bootstrap message.
    stranded: usize,
    /// Faults the launched actors are designed to take.
    expected_faults: usize,
    /// Kernel messages a dead actor's address accepted (must stay zero).
    stale_accepted: usize,
    /// The first actors to die on purpose, kept as dead-address targets.
    crashed: [Option<LocalAddress>; 2],
}

impl Plan {
    fn new() -> Self {
        Self {
            seed: 0x9e37_79b9_7f4a_7c15 ^ read_counter_value(),
            spawned: 0,
            spawn_failures: 0,
            stranded: 0,
            expected_faults: 0,
            stale_accepted: 0,
            crashed: [None; 2],
        }
    }

    fn report(&self, label: &str) {
        information!(
            EarlyConsole,
            "{label}: {} actors, {} spawn failures, {} stranded, {} deliberate faults expected, {} stale messages accepted",
            self.spawned,
            self.spawn_failures,
            self.stranded,
            self.expected_faults,
            self.stale_accepted,
        );
    }

    fn next_seed(&mut self) -> u64 {
        self.seed ^= self.seed << 13;
        self.seed ^= self.seed >> 7;
        self.seed ^= self.seed << 17;
        self.seed
    }

    /// Spawn failures are expected under stress, so they are counted, not fatal.
    fn spawn(&mut self) -> Option<LocalAddress> {
        match spawn_user_actor(EL0_GENESIS_PROGRAM, None, default_configuration()) {
            Ok(actor) => {
                self.spawned += 1;
                Some(actor)
            }
            Err(_) => {
                self.spawn_failures += 1;
                None
            }
        }
    }

    /// Sends the bootstrap message, retrying transient pool pressure.
    fn boot(
        &mut self,
        destination: LocalAddress,
        peer: LocalAddress,
        role: u64,
        scenario: Scenario,
        param: u64,
    ) -> bool {
        let bootstrap = Bootstrap {
            peer: pack_local_address(peer),
            role,
            scenario,
            seed: self.next_seed(),
            param,
        };
        let payload = bootstrap.encode();

        for _ in 0..BOOTSTRAP_ATTEMPTS {
            let metadata = MessageMetadata {
                sender: destination,
                tag: BOOTSTRAP_TAG,
                correlation: 0,
            };
            if tell_actor(destination, metadata, &payload).is_ok() {
                self.expected_faults += bootstrap.expected_faults();
                if scenario != Scenario::QuickExit {
                    information!(EarlyConsole, "Bootstrap {destination}: {scenario:?}");
                }
                return true;
            }
            spin_loop();
        }

        self.stranded += 1;
        warning!(
            EarlyConsole,
            "Bootstrap of {destination} ({scenario:?}) refused after {BOOTSTRAP_ATTEMPTS} attempts"
        );
        false
    }

    fn boot_solo(&mut self, actor: LocalAddress, scenario: Scenario, param: u64) -> bool {
        self.boot(actor, actor, 0, scenario, param)
    }

    /// Selective request/reply pairs; the last pair dies mid-conversation.
    fn request_reply_pairs(&mut self) {
        for die_after in PING_PONG_PAIRS {
            let (first, second) = (self.spawn(), self.spawn());
            let (Some(first), Some(second)) = (first, second) else {
                self.stranded += usize::from(first.is_some()) + usize::from(second.is_some());
                continue;
            };
            // Responder first, so the initiator's first ping finds it ready.
            self.boot(second, first, 1, Scenario::PingPong, die_after);
            self.boot(first, second, 0, Scenario::PingPong, die_after);
        }
    }

    /// Many senders flood one collector that checks per-sender FIFO order.
    fn collector_flood(&mut self) {
        let Some(collector) = self.spawn() else {
            return;
        };
        let mut senders = [None; COLLECTOR_SENDERS];
        for slot in &mut senders {
            *slot = self.spawn();
        }
        let live = senders.iter().flatten().count();

        // The collector must be ready before the senders fill its inbox.
        if !self.boot(
            collector,
            collector,
            live as u64,
            Scenario::Collector,
            COLLECTOR_MESSAGES_PER_SENDER,
        ) {
            self.stranded += live;
            return;
        }
        for (index, sender) in senders.into_iter().flatten().enumerate() {
            self.boot(
                sender,
                collector,
                index as u64,
                Scenario::CollectorSender,
                COLLECTOR_MESSAGES_PER_SENDER,
            );
        }
    }

    /// One server, healthy clients, and clients that die with a request in flight.
    fn server_with_clients(&mut self) {
        let Some(server) = self.spawn() else { return };
        let mut clients = [None; CLIENTS + CRASHING_CLIENTS];
        for slot in &mut clients {
            *slot = self.spawn();
        }
        let healthy = clients[..CLIENTS].iter().flatten().count();
        let crashing = clients[CLIENTS..].iter().flatten().count();
        let expected = healthy as u64 * REQUESTS_PER_CLIENT + crashing as u64;

        // The server must be ready before the clients start sending.
        if !self.boot(server, server, 0, Scenario::Server, expected) {
            self.stranded += healthy + crashing;
            return;
        }

        let mut index = 0;
        for (position, client) in clients.into_iter().enumerate() {
            let Some(client) = client else { continue };
            let (scenario, param) = if position < CLIENTS {
                (Scenario::Client, REQUESTS_PER_CLIENT)
            } else {
                (Scenario::ClientCrash, 0)
            };
            self.boot(client, server, index, scenario, param);
            index += 1;
        }
    }

    /// Self-checking actors that run alongside everything else.
    fn solo_validators(&mut self) {
        for scenario in SOLO_VALIDATORS {
            if let Some(actor) = self.spawn() {
                self.boot_solo(actor, scenario, 0);
            }
        }
    }

    /// Actors that each die a different way. The first two become dead-address targets.
    fn fault_injection(&mut self) {
        for scenario in FAULTS {
            let Some(actor) = self.spawn() else { continue };
            if !self.boot_solo(actor, scenario, 0) {
                continue;
            }
            if let Some(slot) = self.crashed.iter_mut().find(|slot| slot.is_none()) {
                *slot = Some(actor);
            }
        }
    }

    /// CPU hogs: preemption must keep everyone else moving.
    fn cpu_hogs(&mut self) {
        for _ in 0..CPU_HOGS {
            if let Some(actor) = self.spawn() {
                self.boot_solo(actor, Scenario::SpinHog, 0);
            }
        }
    }

    /// Spam the addresses of actors that are dead or dying.
    fn dead_letter_spam(&mut self) {
        let targets = self.crashed;
        for target in targets.into_iter().flatten().take(DEAD_LETTER_SPAMMERS) {
            if let Some(spammer) = self.spawn() {
                self.boot(spammer, target, 0, Scenario::DeadLetter, 0);
            }
        }
    }

    /// Actors that bootstrap and exit immediately.
    fn churn_wave(&mut self) -> usize {
        let mut launched = 0;
        for _ in 0..CHURN_WAVE_SIZE {
            let Some(actor) = self.spawn() else { break };
            if !self.boot_solo(actor, Scenario::QuickExit, 0) {
                break;
            }
            launched += 1;
        }
        launched
    }

    /// Churn waves reuse freed actor slots, exercising slot reuse and generation
    /// checks. Each wave also hammers a dead actor's address from the kernel.
    fn churn_waves(&mut self) {
        let interval = ticks_for(CHURN_INTERVAL_MILLISECONDS);
        let mut deadline = read_counter_value().saturating_add(interval);

        for wave in 1..=CHURN_WAVES {
            wait_until(deadline);
            deadline = deadline.saturating_add(interval);

            let launched = self.churn_wave();
            let accepted = self.crashed[0].map_or(0, |target| hammer(target, DEAD_HAMMER_MESSAGES));
            self.stale_accepted += accepted;

            information!(
                EarlyConsole,
                "Churn wave {wave}/{CHURN_WAVES}: {launched}/{CHURN_WAVE_SIZE} actors launched, {accepted}/{DEAD_HAMMER_MESSAGES} messages accepted by a dead actor"
            );
            if accepted != 0 {
                warning!(
                    EarlyConsole,
                    "A dead actor's address still accepted messages (stale address not rejected)"
                );
            }
        }
    }

    /// With the system otherwise idle, inbox capacity must be identical every cycle.
    fn quiet_leak_check(&mut self) {
        if let Some(actor) = self.spawn() {
            self.boot_solo(actor, Scenario::SelfCycle, STRICT);
        }
        wait_until(read_counter_value().saturating_add(ticks_for(QUIET_PERIOD_MILLISECONDS)));
    }

    /// Permanently exhausts the shared pool. Runs last because nothing that
    /// needs a healthy pool may run beside it.
    fn exhaustion(&mut self) {
        if let Some(idle) = self.spawn() {
            // Never bootstrapped on purpose; the kernel fills its inbox.
            let accepted = hammer(idle, IDLE_HAMMER_MESSAGES);
            if accepted == IDLE_HAMMER_MESSAGES {
                warning!(
                    EarlyConsole,
                    "Idle actor's inbox accepted all {IDLE_HAMMER_MESSAGES} messages (unbounded?)"
                );
            } else {
                information!(
                    EarlyConsole,
                    "Idle actor's inbox accepted {accepted}/{IDLE_HAMMER_MESSAGES} before refusing"
                );
            }
        }

        let mut orphans = [None; ORPHANS];
        for slot in &mut orphans {
            *slot = self.spawn();
        }
        for orphan in orphans.into_iter().flatten() {
            self.boot_solo(orphan, Scenario::BlockForever, 0);
        }
        // Fillers' own bootstraps can be refused once the pool is full; that is
        // the expected outcome here and is reported after the core line.
        for orphan in orphans.into_iter().flatten() {
            for _ in 0..FILLERS_PER_ORPHAN {
                if let Some(filler) = self.spawn() {
                    self.boot(filler, orphan, 0, Scenario::FillVictim, 0);
                }
            }
        }
    }
}

/// Kernel-originated junk to `target`; returns how many messages were accepted.
fn hammer(target: LocalAddress, count: usize) -> usize {
    let mut accepted = 0;
    let mut first_refusal = None;

    for correlation in 0..count {
        let metadata = MessageMetadata {
            sender: target,
            tag: HAMMER_TAG,
            correlation: correlation as u64,
        };
        match tell_actor(target, metadata, b"kernel hammer") {
            Ok(_) => accepted += 1,
            Err(error) => {
                first_refusal.get_or_insert(error);
            }
        }
    }

    if let Some(error) = first_refusal {
        information!(EarlyConsole, "Hammer on {target}: first refusal {error:?}");
    }
    accepted
}

/// Generic-timer frequency. Prefers the hardware register over a board constant.
fn counter_frequency() -> u64 {
    let frequency: u64;
    // SAFETY: CNTFRQ_EL0 is readable at EL1 and reading it has no side effects.
    unsafe {
        core::arch::asm!("mrs {}, cntfrq_el0", out(reg) frequency,
            options(nomem, nostack, preserves_flags));
    }
    if frequency == 0 {
        FALLBACK_COUNTER_FREQUENCY
    } else {
        frequency
    }
}

fn ticks_for(milliseconds: u64) -> u64 {
    counter_frequency() / 1000 * milliseconds
}

/// Sleeps the boot core until the counter reaches `deadline`.
fn wait_until(deadline: u64) {
    while read_counter_value() < deadline {
        processor::wait_for_event();
    }
}
