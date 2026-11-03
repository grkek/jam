//! First Rust code executed on the boot core.

use foundation::{
    address::LocalAddress, configuration::Configuration,
    device_tree::PowerStateCoordinationInterfaceConduit,
};

use super::{device_tree::load_device_tree, secondary_cores::start_secondary_cores};
use crate::{
    actors::{
        lifecycle::{initialise_reclamation_stack_tracking, reclamation_stack_high_water_bytes},
        message_pool::MessagePool,
        message_storage::{
            ACTOR_INBOX_BYTE_BUDGET, ACTOR_INBOX_CAPACITY, MESSAGE_MAXIMUM_PAYLOAD_BYTES,
            MESSAGE_POOL_CAPACITY,
        },
        table::spawn_user_actor,
    },
    architecture::aarch64::{
        early_console::EarlyConsole,
        exceptions::install_exception_vectors,
        interrupt_controller::initialise_distributor,
        power_interface, processor,
        time::{read_counter_value, record_boot_ticks},
    },
    error, information,
    memory::{
        frame_allocator::{
            FrameAllocator, initialise_frame_allocator, install_frame_allocator,
            with_frame_allocator,
        },
        kernel_image::{kernel_image_end_address, kernel_image_start_address},
        kernel_map::enable_kernel_address_space,
        user_memory::run_user_mapping_self_test,
        user_program::EL0_GENESIS_PROGRAM,
    },
    platform::current::initialise_console_hardware,
    scheduling::{
        core_local::{CORE_MAXIMUM, register_current_core},
        idle::{initialise_interrupt_stack_tracking, install_idle_frames},
        scheduler::start_current_core_tick,
    },
    warning,
};

const BYTES_PER_MEGABYTE: u64 = 1024 * 1024;
const SELF_TEST_FRAME_COUNT: usize = 2;

const BOOTSTRAP_BYTES: usize = 40;
const CHURN_WAVE_SIZE: usize = 32;
const CHURN_WAVES: usize = 8;

/// Scenario identifiers understood by the EL0 stress program. Keep in sync.
mod scenario {
    pub const PING_PONG: u64 = 0;
    pub const COLLECTOR_SENDER: u64 = 1;
    pub const COLLECTOR: u64 = 2;
    pub const SELF_CYCLE: u64 = 3;
    pub const SIZE_PROBE: u64 = 4;
    pub const SYSCALL_FUZZ: u64 = 5;
    pub const SERVER: u64 = 6;
    pub const CLIENT: u64 = 7;
    pub const CLIENT_CRASH: u64 = 8;
    pub const QUICK_EXIT: u64 = 9;
    pub const SPIN_HOG: u64 = 10;
    pub const DEAD_LETTER: u64 = 11;
    pub const FILL_VICTIM: u64 = 12;
    pub const BLOCK_FOREVER: u64 = 13;
    pub const CRASH_BRK: u64 = 20;
    pub const CRASH_NULL: u64 = 21;
    pub const CRASH_EXEC_DATA: u64 = 22;
    pub const CRASH_UNDEFINED: u64 = 23;
    pub const CRASH_PANIC: u64 = 24;
    pub const CRASH_STACK: u64 = 25;
    pub const CRASH_WRITE_CODE: u64 = 26;
}

/// Powers the machine off through the firmware, or parks the core when there is no way to ask.
fn power_off_or_park(conduit: Option<PowerStateCoordinationInterfaceConduit>) -> ! {
    match conduit {
        Some(conduit) => power_interface::system_off(conduit),
        None => loop {
            processor::wait_for_event();
        },
    }
}

/// Allocates and releases frames to prove the allocator still works with the MMU on.
fn run_frame_allocator_self_test(frame_allocator: &mut FrameAllocator) {
    information!(
        EarlyConsole,
        "Running self-test: verifying frame allocation and deallocation integrity..."
    );

    let initial_free_count = frame_allocator.free_frame_count();
    let first_frame = frame_allocator.allocate_frame();
    let second_frame = frame_allocator.allocate_frame();

    let (Some(first_frame), Some(second_frame)) = (first_frame, second_frame) else {
        error!(
            EarlyConsole,
            "Self-test failed: unable to allocate test frames"
        );
        return;
    };

    information!(
        EarlyConsole,
        "Self-test allocation passed: acquired frames at {first_frame:#x} and {second_frame:#x}"
    );

    if frame_allocator.free_frame_count()
        != initial_free_count.saturating_sub(SELF_TEST_FRAME_COUNT)
    {
        warning!(
            EarlyConsole,
            "Self-test warning: frame count mismatch after allocation"
        );
    }

    frame_allocator
        .free_frame(first_frame)
        .expect("Failed to release first test frame");

    frame_allocator
        .free_frame(second_frame)
        .expect("Failed to release second test frame");

    if frame_allocator.free_frame_count() == initial_free_count {
        information!(
            EarlyConsole,
            "Self-test deallocation passed: all test frames successfully returned to pool"
        );
    } else {
        error!(
            EarlyConsole,
            "Self-test error: frame count did not match after release"
        );
    }
}

/// Called by the assembly entry once the stack is ready and `.bss` is cleared.
#[unsafe(no_mangle)]
pub extern "C" fn kernel_main(device_tree_address: usize) -> ! {
    install_exception_vectors();

    let boot_ticks = read_counter_value();
    record_boot_ticks(boot_ticks);

    initialise_console_hardware();

    if !register_current_core(0) {
        error!(
            EarlyConsole,
            "Fatal: unable to register the current core: 0"
        );

        // The device tree has not been read, so the power interface is not known yet.
        power_off_or_park(None);
    }

    // Every core needs a real frame to resume when the run queue is empty.
    install_idle_frames();

    information!(
        EarlyConsole,
        concat!(
            "JAM a.k.a Just Another Microkernel version ",
            env!("CARGO_PKG_VERSION"),
            " (aarch64-unknown-none) #1 SMP"
        )
    );

    information!(
        EarlyConsole,
        "Exception vector table installed successfully"
    );

    information!(
        EarlyConsole,
        "Kernel executing in the upper half, kernel_main at {:#x}",
        kernel_main as usize
    );

    let device_tree = match load_device_tree(device_tree_address) {
        Ok(device_tree) => device_tree,
        Err(error) => {
            error!(EarlyConsole, "Fatal: device tree unusable: {error:?}");

            power_off_or_park(None);
        }
    };
    let summary = device_tree.summary;
    // Discovery is diagnostic in Phase 2; the memory allocator and boot MMU are unchanged.
    match &device_tree.hardware_description {
        Ok(hardware) => {
            information!(
                EarlyConsole,
                "Hardware discovery: {} RAM regions, {} reserved regions, {} CPU affinities, {} dynamic reservation requests",
                hardware.memory_regions().len(),
                hardware.reserved_regions().len(),
                hardware.cpu_identifiers().len(),
                hardware.dynamic_reservation_count(),
            );
            for (index, region) in hardware.memory_regions().iter().enumerate() {
                information!(
                    EarlyConsole,
                    "Discovered RAM {index}: {:#x}..{:#x}",
                    region.start_address,
                    region.end_address(),
                );
            }
            for (index, reservation) in hardware.reserved_regions().iter().enumerate() {
                information!(
                    EarlyConsole,
                    "Discovered reservation {index}: {:#x}..{:#x} ({:?})",
                    reservation.region.start_address,
                    reservation.region.end_address(),
                    reservation.source,
                );
            }
            for (index, identifier) in hardware.cpu_identifiers().iter().enumerate() {
                information!(
                    EarlyConsole,
                    "Discovered CPU {index}: affinity {identifier:#x}"
                );
            }
            if hardware.dynamic_reservation_count() != 0 {
                warning!(
                    EarlyConsole,
                    "Dynamic reserved-memory requests are not allocated in Phase 2",
                );
            }
        }
        Err(error) => warning!(
            EarlyConsole,
            "Hardware discovery unavailable: {error:?}; refusing physical memory allocation",
        ),
    }

    let conduit = summary.power_state_coordination_interface_conduit;

    // Firmware discovery is now mandatory: allocating on an unknown map is unsafe.
    let hardware = match device_tree.hardware_description {
        Ok(hardware) => hardware,
        Err(error) => {
            error!(EarlyConsole, "Fatal: hardware discovery failed: {error:?}");
            power_off_or_park(conduit);
        }
    };

    power_interface::record_conduit(conduit);

    information!(
        EarlyConsole,
        "Device tree blob discovered at physical address {:#x}",
        device_tree.blob_address
    );

    let memory_end = summary.memory_base_address + summary.memory_byte_count;

    information!(
        EarlyConsole,
        "Physical memory map: {:#x} to {:#x}, {} bytes ({} megabytes)",
        summary.memory_base_address,
        memory_end,
        summary.memory_byte_count,
        summary.memory_byte_count / BYTES_PER_MEGABYTE
    );

    information!(
        EarlyConsole,
        "Kernel image occupies {:#x} to {:#x}",
        kernel_image_start_address(),
        kernel_image_end_address()
    );

    information!(
        EarlyConsole,
        "Multiprocessing: {} execution cores detected; power state coordination interface: {:?}",
        summary.central_processing_unit_count,
        conduit
    );

    let mut frame_allocator = match initialise_frame_allocator(
        &summary,
        &hardware,
        device_tree.blob_address,
        device_tree.blob_byte_count,
    ) {
        Ok(frame_allocator) => frame_allocator,
        Err(error) => {
            error!(
                EarlyConsole,
                "Fatal: unsafe physical memory layout: {error:?}"
            );
            power_off_or_park(conduit);
        }
    };

    information!(
        EarlyConsole,
        "Frame allocator initialised: {} of {} physical frames available",
        frame_allocator.free_frame_count(),
        frame_allocator.total_frame_count()
    );

    let kernel_address_space = match enable_kernel_address_space(&summary, &mut frame_allocator) {
        Ok(kernel_address_space) => kernel_address_space,
        Err(error) => {
            error!(
                EarlyConsole,
                "Fatal: kernel address space unusable: {error:?}"
            );

            power_off_or_park(conduit);
        }
    };

    information!(
        EarlyConsole,
        "Memory management unit enabled: translation tables at {:#x}, caches on, write-xor-execute enforced",
        kernel_address_space.root_table_address()
    );

    run_frame_allocator_self_test(&mut frame_allocator);

    // From here on the kernel allocates memory through the shared allocator.
    install_frame_allocator(frame_allocator);

    match with_frame_allocator(run_user_mapping_self_test) {
        Some(Ok(())) => information!(EarlyConsole, "User-mapping self-test passed"),
        Some(Err(reason)) => {
            error!(EarlyConsole, "User-mapping self-test failed: {reason}");

            power_off_or_park(conduit);
        }
        None => error!(
            EarlyConsole,
            "User-mapping self-test skipped: frame allocator not installed"
        ),
    }

    initialise_interrupt_stack_tracking();
    initialise_reclamation_stack_tracking();

    // Verify that all reclamation stacks start with an intact pattern.
    for core_index in 0..CORE_MAXIMUM {
        let (used, capacity) = reclamation_stack_high_water_bytes(core_index);

        assert_eq!(
            used, 0,
            "Reclamation stack tracking invalid on core {core_index}: \
         {used} / {capacity} bytes already used"
        );
    }

    initialise_distributor();

    // Secondary cores and the timer must be live before an actor can finish and land in idle.
    if crate::platform::current::SECONDARY_CORE_STARTUP_SUPPORTED {
        start_secondary_cores(
            &summary,
            &hardware,
            kernel_address_space.root_table_address(),
        );
    } else {
        warning!(
            EarlyConsole,
            "Secondary-core startup is not yet implemented for this platform"
        );
    }

    start_current_core_tick();

    let charge = MessagePool::<(), MESSAGE_POOL_CAPACITY, MESSAGE_MAXIMUM_PAYLOAD_BYTES, 0>::message_charge::<LocalAddress>().expect("charge").bytes();
    assert!(
        charge * ACTOR_INBOX_CAPACITY <= ACTOR_INBOX_BYTE_BUDGET,
        "inbox budget too small: charge {charge} x {ACTOR_INBOX_CAPACITY} > {ACTOR_INBOX_BYTE_BUDGET}"
    );

    information!(
        EarlyConsole,
        "Kernel bootstrap sequence complete. Handing control over to the scheduler..."
    );

    information!(EarlyConsole, "Genesis bootstrap: beginning actor creation");

    information!(
        EarlyConsole,
        "Genesis bootstrap: beginning stress-test actor creation"
    );

    let mut seed: u64 = 0x9e37_79b9_7f4a_7c15 ^ (read_counter_value() as u64);
    let mut spawned = 0_usize;
    let mut spawn_failures = 0_usize;
    let mut bootstrap_failures = 0_usize;

    // Spawn failures are expected under stress (table exhaustion), so they are
    // counted rather than fatal.
    macro_rules! spawn {
        () => {
            match spawn_user_actor(EL0_GENESIS_PROGRAM, None, Configuration::default()) {
                Ok(actor) => {
                    spawned += 1;
                    Some(actor)
                }
                Err(_) => {
                    spawn_failures += 1;
                    None
                }
            }
        };
    }

    // Sends the five-word bootstrap message; evaluates to whether it was delivered.
    macro_rules! boot {
        ($destination:expr, $peer:expr, $role:expr, $scenario:expr, $param:expr) => {{
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            let destination = $destination;
            let packed_peer = crate::system_calls::actor_calls::pack_local_address($peer);
            let mut payload = [0_u8; BOOTSTRAP_BYTES];
            for (slot, value) in
                payload
                    .chunks_exact_mut(8)
                    .zip([packed_peer, $role, $scenario, seed, $param])
            {
                slot.copy_from_slice(&value.to_le_bytes());
            }
            let delivered = crate::messaging::delivery::tell_actor(
                destination,
                crate::actors::inbox::MessageMetadata {
                    sender: destination,
                    tag: 0,
                    correlation: 0,
                },
                &payload,
            )
            .is_ok();
            if !delivered {
                bootstrap_failures += 1;
            }
            delivered
        }};
    }

    // Kernel-originated junk to an address; evaluates to how many sends were accepted.
    macro_rules! hammer {
        ($target:expr, $count:expr) => {{
            let target = $target;
            let mut accepted = 0_usize;
            for n in 0..$count {
                if crate::messaging::delivery::tell_actor(
                    target,
                    crate::actors::inbox::MessageMetadata {
                        sender: target,
                        tag: 3,
                        correlation: n as u64,
                    },
                    b"kernel hammer",
                )
                .is_ok()
                {
                    accepted += 1;
                }
            }
            accepted
        }};
    }

    // Spawn/exit churn: actors that bootstrap and finish immediately.
    macro_rules! churn_wave {
        () => {{
            let mut launched = 0_usize;
            for _ in 0..CHURN_WAVE_SIZE {
                if let Some(actor) = spawn!() {
                    if boot!(actor, actor, 0_u64, scenario::QUICK_EXIT, 0_u64) {
                        launched += 1;
                    }
                }
            }
            launched
        }};
    }

    // A. Request/reply pairs: two healthy, one that dies mid-conversation and
    //    leaves its partner blocked forever.
    for die_after in [0_u64, 0, 40] {
        if let (Some(a), Some(b)) = (spawn!(), spawn!()) {
            boot!(a, b, 0_u64, scenario::PING_PONG, die_after);
            boot!(b, a, 1_u64, scenario::PING_PONG, die_after);
        }
    }

    // B. Many-to-one flood: six senders, one collector checking per-sender FIFO.
    const COLLECTOR_SENDERS: usize = 6;
    const COLLECTOR_COUNT: u64 = 400;
    if let Some(collector) = spawn!() {
        let mut launched = 0_u64;
        for _ in 0..COLLECTOR_SENDERS {
            if let Some(sender) = spawn!() {
                if boot!(
                    sender,
                    collector,
                    launched,
                    scenario::COLLECTOR_SENDER,
                    COLLECTOR_COUNT
                ) {
                    launched += 1;
                }
            }
        }
        boot!(
            collector,
            collector,
            launched,
            scenario::COLLECTOR,
            COLLECTOR_COUNT
        );
    }

    // C. One server, healthy clients, and clients that die with a request in flight.
    const CLIENTS: usize = 4;
    const CRASHING_CLIENTS: usize = 2;
    const CLIENT_REQUESTS: u64 = 200;
    if let Some(server) = spawn!() {
        let mut expected = 0_u64;
        let mut index = 0_u64;
        for _ in 0..CLIENTS {
            if let Some(client) = spawn!() {
                if boot!(client, server, index, scenario::CLIENT, CLIENT_REQUESTS) {
                    expected += CLIENT_REQUESTS;
                    index += 1;
                }
            }
        }
        for _ in 0..CRASHING_CLIENTS {
            if let Some(client) = spawn!() {
                if boot!(client, server, index, scenario::CLIENT_CRASH, 0_u64) {
                    expected += 1;
                    index += 1;
                }
            }
        }
        boot!(server, server, 0_u64, scenario::SERVER, expected);
    }

    // D. Solo validators running alongside everything else.
    for solo in [
        scenario::SELF_CYCLE,
        scenario::SELF_CYCLE,
        scenario::SIZE_PROBE,
        scenario::SIZE_PROBE,
        scenario::SYSCALL_FUZZ,
        scenario::SYSCALL_FUZZ,
    ] {
        if let Some(actor) = spawn!() {
            boot!(actor, actor, 0_u64, solo, 0_u64);
        }
    }

    // E. Fault injection: each dies a different way. Keep a few addresses to hit later.
    let mut crashers = [None; 4];
    let mut crasher_count = 0_usize;
    for fault in [
        scenario::CRASH_BRK,
        scenario::CRASH_NULL,
        scenario::CRASH_EXEC_DATA,
        scenario::CRASH_UNDEFINED,
        scenario::CRASH_PANIC,
        scenario::CRASH_STACK,
        scenario::CRASH_WRITE_CODE,
    ] {
        if let Some(actor) = spawn!() {
            boot!(actor, actor, 0_u64, fault, 0_u64);
            if crasher_count < crashers.len() {
                crashers[crasher_count] = Some(actor);
                crasher_count += 1;
            }
        }
    }

    // F. CPU hogs: preemption must keep everyone else moving.
    for _ in 0..2 {
        if let Some(actor) = spawn!() {
            boot!(actor, actor, 0_u64, scenario::SPIN_HOG, 0_u64);
        }
    }

    // G. Orphans that never receive, each with fillers pushing until the inbox refuses.
    for _ in 0..2 {
        if let Some(orphan) = spawn!() {
            boot!(orphan, orphan, 0_u64, scenario::BLOCK_FOREVER, 0_u64);
            for _ in 0..2 {
                if let Some(filler) = spawn!() {
                    boot!(filler, orphan, 0_u64, scenario::FILL_VICTIM, 0_u64);
                }
            }
        }
    }

    // H. Spam addresses of actors that are dead or dying.
    for target in crashers.into_iter().flatten().take(2) {
        if let Some(spammer) = spawn!() {
            boot!(spammer, target, 0_u64, scenario::DEAD_LETTER, 0_u64);
        }
    }

    // I. An actor that never gets a bootstrap message, with the kernel filling its inbox.
    let unbootstrapped = spawn!();
    if let Some(actor) = unbootstrapped {
        let accepted = hammer!(actor, 4096);
        if accepted == 4096 {
            warning!(
                EarlyConsole,
                "Kernel hammer: inbox of an idle actor accepted all 4096 messages (unbounded?)"
            );
        } else {
            information!(
                EarlyConsole,
                "Kernel hammer: idle actor inbox accepted {accepted}/4096 before refusing"
            );
        }
    }

    // J. First churn wave.
    let churned = churn_wave!();

    information!(
        EarlyConsole,
        "Stress plan launched: {spawned} actors, {spawn_failures} spawn failures, {bootstrap_failures} bootstrap failures, {churned} churn actors"
    );

    // Further churn waves reuse freed actor slots (stale-address and slot-reuse paths).
    // The timing uses the raw counter; 62.5 MHz (QEMU virt) gives roughly one second.
    let wave_interval = 62_500_000;
    let mut next_wave = read_counter_value().saturating_add(wave_interval);
    let mut waves = 0_usize;

    loop {
        processor::wait_for_event();

        if waves < CHURN_WAVES && read_counter_value() >= next_wave {
            waves += 1;
            next_wave = next_wave.saturating_add(wave_interval);

            let churned = churn_wave!();
            let dead_accepted = match crashers[0] {
                Some(target) => hammer!(target, 256),
                None => 0,
            };

            information!(
                EarlyConsole,
                "Churn wave {waves}/{CHURN_WAVES}: {churned} actors launched, {dead_accepted}/256 kernel messages accepted by a dead actor"
            );
            if dead_accepted != 0 {
                warning!(
                    EarlyConsole,
                    "A dead actor's address still accepted messages (stale address not rejected)"
                );
            }
        }
    }
}
