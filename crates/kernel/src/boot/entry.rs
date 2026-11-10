//! First Rust code executed on the boot core.

use foundation::{
    address::LocalAddress,
    device_tree::PowerStateCoordinationInterfaceConduit,
    hardware_description::HardwareDescription,
    resource_budget::{
        Capacity, Limit, MAXIMUM_INBOX_BYTE_COUNT, MAXIMUM_INBOX_MESSAGE_COUNT,
        MAXIMUM_MESSAGE_PAYLOAD_BYTE_COUNT, MAXIMUM_POOL_MESSAGE_COUNT,
    },
};

use super::{device_tree::load_device_tree, secondary_cores::start_secondary_cores};

#[cfg(feature = "stress")]
use super::stress;

use crate::{
    actors::{
        divider,
        lifecycle::{initialise_reclamation_stack_tracking, reclamation_stack_high_water_bytes},
        message_pool::MessagePool,
        table::{ACTOR_SLOT_BYTE_COUNT, initialise_actor_table},
    },
    architecture::aarch64::{
        early_console::EarlyConsole,
        exceptions::install_exception_vectors,
        interrupt_controller::initialise_distributor,
        memory_management_unit::actor_slot_ceiling,
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
        kernel_stack::initialise_kernel_stacks,
        user_memory::run_user_mapping_self_test,
    },
    platform::current::initialise_console_hardware,
    scheduling::{
        core_local::{CORE_COUNT, initialise_queues, register_current_core},
        idle::{initialise_interrupt_stack_tracking, install_idle_frames},
        scheduler::start_current_core_tick,
    },
    warning,
};

/// Bytes in one megabyte, as reported in the boot log.
const MEGABYTE_BYTE_COUNT: u64 = 1024 * 1024;

/// Frames the allocator self-test holds at once: one first frame and one second frame.
const SELF_TEST_ALLOCATED_FRAME_COUNT: usize = 2;

/// Logs an error, then powers off (or parks) the machine.
macro_rules! fatal {
    ($conduit:expr, $($arguments:tt)*) => {{
        error!(EarlyConsole, $($arguments)*);
        power_off_or_park($conduit)
    }};
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

/// Logs the kernel version and where `kernel_main` landed in the upper half.
fn banner() {
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
        "Kernel executing in the upper half, kernel_main at {:#x}",
        kernel_main as usize
    );
}

/// Brings the boot core to the point where it can log, take exceptions, and idle.
fn start_boot_core() {
    install_exception_vectors();
    record_boot_ticks(read_counter_value());
    initialise_console_hardware();

    if !register_current_core(0) {
        // The device tree has not been read, so the power interface is not known yet.
        fatal!(None, "Fatal: unable to register the current core: 0");
    }

    // Every core needs a real frame to resume when the run queue is empty.
    install_idle_frames();
    banner();
}

/// Logs what the device tree says about memory, reservations and CPUs.
fn report_hardware_description(hardware: &HardwareDescription) {
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

/// Logs the physical memory range the device tree describes, and where the kernel image sits.
fn report_physical_memory(memory_base_address: u64, memory_byte_count: u64) {
    information!(
        EarlyConsole,
        "Physical memory map: {:#x} to {:#x}, {} bytes ({} megabytes)",
        memory_base_address,
        memory_base_address.saturating_add(memory_byte_count),
        memory_byte_count,
        memory_byte_count / MEGABYTE_BYTE_COUNT
    );
    information!(
        EarlyConsole,
        "Kernel image occupies {:#x} to {:#x}",
        kernel_image_start_address(),
        kernel_image_end_address()
    );
}

/// Allocates and releases frames to prove the allocator still works with the MMU on.
fn run_frame_allocator_self_test(frame_allocator: &mut FrameAllocator) {
    information!(
        EarlyConsole,
        "Running self-test: verifying frame allocation and deallocation integrity..."
    );

    let initial_free_frame_count = frame_allocator.free_frame_count();
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

    let expected_free_frame_count =
        initial_free_frame_count.saturating_sub(SELF_TEST_ALLOCATED_FRAME_COUNT);
    let actual_free_frame_count = frame_allocator.free_frame_count();
    if actual_free_frame_count != expected_free_frame_count {
        warning!(
            EarlyConsole,
            "Self-test warning: frame count mismatch after allocation: expected {expected_free_frame_count} free, found {actual_free_frame_count} (started with {initial_free_frame_count})"
        );
    }

    frame_allocator
        .free_frame(first_frame)
        .expect("Failed to release first test frame");
    frame_allocator
        .free_frame(second_frame)
        .expect("Failed to release second test frame");

    if frame_allocator.free_frame_count() == initial_free_frame_count {
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

/// Returns the bytes one queued message is charged against an inbox.
fn message_charge_byte_count() -> usize {
    MessagePool::<
        (),
        MAXIMUM_POOL_MESSAGE_COUNT,
        MAXIMUM_MESSAGE_PAYLOAD_BYTE_COUNT,
        0,
    >::message_charge::<LocalAddress>()
    .expect("The message charge must be computable for a local address")
    .bytes()
}

/// Measures what this machine offers, so the limits follow the hardware instead of constants.
fn measure_system_capacity(frame_allocator: &FrameAllocator) -> Capacity {
    Capacity::with_kernel_message_limits(
        frame_allocator.free_frame_count(),
        actor_slot_ceiling(),
        ACTOR_SLOT_BYTE_COUNT,
        message_charge_byte_count(),
    )
}

/// A full inbox must fit within its byte budget, and within the storage and ceilings behind it.
fn verify_inbox_budget(capacity: &Capacity, limit: &Limit) {
    let message_charge_byte_count = capacity.message_charge_byte_count;
    let inbox_message_budget =
        usize::try_from(limit.budget.inbox_message_budget).unwrap_or(usize::MAX);
    let inbox_byte_budget = usize::try_from(limit.budget.inbox_byte_budget).unwrap_or(usize::MAX);

    assert!(
        inbox_message_budget <= capacity.inbox_message_capacity,
        "Inbox message budget {inbox_message_budget} exceeds the inbox storage {}",
        capacity.inbox_message_capacity
    );
    assert!(
        inbox_message_budget <= MAXIMUM_INBOX_MESSAGE_COUNT,
        "Inbox message budget {inbox_message_budget} exceeds the ceiling {MAXIMUM_INBOX_MESSAGE_COUNT}"
    );
    assert!(
        inbox_byte_budget <= MAXIMUM_INBOX_BYTE_COUNT,
        "Inbox byte budget {inbox_byte_budget} exceeds the ceiling {MAXIMUM_INBOX_BYTE_COUNT}"
    );
    assert!(
        message_charge_byte_count.saturating_mul(inbox_message_budget) <= inbox_byte_budget,
        "Inbox budget too small: charge {message_charge_byte_count} x {inbox_message_budget} > {inbox_byte_budget}"
    );
}

/// Derives the system limit and builds every table that holds one entry per actor slot.
///
/// The tables take their frames straight from the allocator, so this must run while the
/// boot code still owns it, before it is installed for shared use.
fn build_actor_storage(
    frame_allocator: &mut FrameAllocator,
    conduit: Option<PowerStateCoordinationInterfaceConduit>,
) {
    // The limits follow the memory left after the kernel has mapped itself.
    let capacity = measure_system_capacity(frame_allocator);
    let limit = match Limit::derive(capacity) {
        Ok(limit) => limit,
        Err(error) => fatal!(conduit, "Fatal: system limit unusable: {error:?}"),
    };
    verify_inbox_budget(&capacity, &limit);

    // The limit is installed for the whole kernel, so every later spawn is checked against it.
    match divider::install(limit) {
        Ok(()) => information!(
            EarlyConsole,
            "System limit divider installed: {} actors, {} pages, {} messages and {} bytes per inbox",
            limit.maximum_actors,
            limit.budget.memory_budget_in_pages,
            limit.budget.inbox_message_budget,
            limit.budget.inbox_byte_budget,
        ),
        Err(error) => fatal!(
            conduit,
            "Fatal: system limit divider installation failed: {error:?}"
        ),
    }

    match initialise_actor_table(frame_allocator, limit.maximum_actors) {
        Ok(frame_count) => information!(
            EarlyConsole,
            "Actor table built: {} slots in {frame_count} frames",
            limit.maximum_actors
        ),
        Err(error) => fatal!(conduit, "Fatal: actor table unusable: {error:?}"),
    }

    // Every actor slot needs a kernel stack, so they are built with the table.
    match initialise_kernel_stacks(frame_allocator, limit.maximum_actors) {
        Ok(frame_count) => information!(
            EarlyConsole,
            "Kernel stacks built: {} stacks in {frame_count} frames",
            limit.maximum_actors
        ),
        Err(error) => fatal!(conduit, "Fatal: kernel stacks unusable: {error:?}"),
    }

    // Every core can hold every actor in its run queue, so the queues are built with the table.
    match initialise_queues(frame_allocator, limit.maximum_actors) {
        Ok(frame_count) => information!(
            EarlyConsole,
            "Run queues built: {CORE_COUNT} queues in {frame_count} frames"
        ),
        Err(error) => fatal!(conduit, "Fatal: run queues unusable: {error:?}"),
    }
}

/// Maps a user address space through the installed allocator to prove it works end to end.
fn run_user_mapping_check(conduit: Option<PowerStateCoordinationInterfaceConduit>) {
    match with_frame_allocator(run_user_mapping_self_test) {
        Some(Ok(())) => information!(EarlyConsole, "User-mapping self-test passed"),
        Some(Err(reason)) => fatal!(conduit, "User-mapping self-test failed: {reason}"),
        None => error!(
            EarlyConsole,
            "User-mapping self-test skipped: frame allocator not installed"
        ),
    }
}

/// Verifies that all reclamation stacks start with an intact pattern.
fn verify_reclamation_stacks() {
    for core_index in 0..CORE_COUNT {
        let (used_byte_count, capacity_byte_count) = reclamation_stack_high_water_bytes(core_index);

        assert_eq!(
            used_byte_count, 0,
            "Reclamation stack tracking invalid on core {core_index}: \
             {used_byte_count} / {capacity_byte_count} bytes already used"
        );
    }
}

/// Starts tracking stack usage on interrupt and reclamation stacks, and checks the starting state.
fn initialise_stack_tracking() {
    initialise_interrupt_stack_tracking();
    initialise_reclamation_stack_tracking();
    verify_reclamation_stacks();
}

/// With the stress plan compiled in, run it and power off so CI can see the exit.
#[cfg(feature = "stress")]
fn after_bootstrap(conduit: Option<PowerStateCoordinationInterfaceConduit>) -> ! {
    stress::run();
    // QEMU `virt` powers off here. Boards without a power interface park the core.
    power_off_or_park(conduit)
}

/// Starts Genesis, the terminal, and the temporary polling console driver.
#[cfg(not(feature = "stress"))]
fn after_bootstrap(conduit: Option<PowerStateCoordinationInterfaceConduit>) -> ! {
    if let Err(reason) = super::console::start() {
        fatal!(conduit, "Userland bootstrap failed: {reason}");
    }
    loop {
        processor::wait_for_event();
    }
}

/// Called by the assembly entry once the stack is ready and `.bss` is cleared.
#[unsafe(no_mangle)]
pub extern "C" fn kernel_main(device_tree_address: usize) -> ! {
    start_boot_core();

    let device_tree = match load_device_tree(device_tree_address) {
        Ok(device_tree) => device_tree,
        Err(error) => fatal!(None, "Fatal: device tree unusable: {error:?}"),
    };
    let summary = device_tree.summary;
    let conduit = summary.power_state_coordination_interface_conduit;

    // Firmware discovery is mandatory: allocating on an unknown map is unsafe.
    let hardware = match device_tree.hardware_description {
        Ok(hardware) => hardware,
        Err(error) => fatal!(
            conduit,
            "Fatal: hardware discovery failed: {error:?}; refusing physical memory allocation"
        ),
    };
    // Discovery is diagnostic in Phase 2; the memory allocator and boot MMU are unchanged.
    report_hardware_description(&hardware);

    power_interface::record_conduit(conduit);

    information!(
        EarlyConsole,
        "Device tree blob discovered at physical address {:#x}",
        device_tree.blob_address
    );
    report_physical_memory(summary.memory_base_address, summary.memory_byte_count);
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
        Err(error) => fatal!(conduit, "Fatal: unsafe physical memory layout: {error:?}"),
    };

    information!(
        EarlyConsole,
        "Frame allocator initialised: {} of {} physical frames available",
        frame_allocator.free_frame_count(),
        frame_allocator.total_frame_count()
    );

    let kernel_address_space = match enable_kernel_address_space(&summary, &mut frame_allocator) {
        Ok(kernel_address_space) => kernel_address_space,
        Err(error) => fatal!(conduit, "Fatal: kernel address space unusable: {error:?}"),
    };

    information!(
        EarlyConsole,
        "Memory management unit enabled: translation tables at {:#x}, caches on, write-xor-execute enforced",
        kernel_address_space.root_table_address()
    );

    run_frame_allocator_self_test(&mut frame_allocator);

    build_actor_storage(&mut frame_allocator, conduit);

    // From here on the kernel allocates memory through the shared allocator.
    install_frame_allocator(frame_allocator);

    run_user_mapping_check(conduit);
    initialise_stack_tracking();

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

    information!(
        EarlyConsole,
        "Kernel bootstrap sequence complete. Handing control over to the scheduler..."
    );

    after_bootstrap(conduit)
}
