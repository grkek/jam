//! First Rust code executed on the boot core.

use foundation::{
    address::LocalAddress, device_tree::PowerStateCoordinationInterfaceConduit,
    hardware_description::HardwareDescription,
};

use super::{device_tree::load_device_tree, secondary_cores::start_secondary_cores};

#[cfg(feature = "stress")]
use super::stress;

use crate::{
    actors::{
        lifecycle::{initialise_reclamation_stack_tracking, reclamation_stack_high_water_bytes},
        message_pool::MessagePool,
        message_storage::{
            ACTOR_INBOX_BYTE_BUDGET, ACTOR_INBOX_CAPACITY, MESSAGE_MAXIMUM_PAYLOAD_BYTES,
            MESSAGE_POOL_CAPACITY,
        },
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

fn log_banner() {
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

/// Verifies that all reclamation stacks start with an intact pattern.
fn verify_reclamation_stacks() {
    for core_index in 0..CORE_MAXIMUM {
        let (used, capacity) = reclamation_stack_high_water_bytes(core_index);

        assert_eq!(
            used, 0,
            "Reclamation stack tracking invalid on core {core_index}: \
             {used} / {capacity} bytes already used"
        );
    }
}

/// A full inbox must fit within its byte budget.
fn verify_inbox_budget() {
    let charge = MessagePool::<(), MESSAGE_POOL_CAPACITY, MESSAGE_MAXIMUM_PAYLOAD_BYTES, 0>::message_charge::<LocalAddress>()
        .expect("charge")
        .bytes();
    assert!(
        charge * ACTOR_INBOX_CAPACITY <= ACTOR_INBOX_BYTE_BUDGET,
        "inbox budget too small: charge {charge} x {ACTOR_INBOX_CAPACITY} > {ACTOR_INBOX_BYTE_BUDGET}"
    );
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
        error!(EarlyConsole, "Userland bootstrap failed: {reason}");
        power_off_or_park(conduit);
    }
    loop {
        processor::wait_for_event();
    }
}

/// Called by the assembly entry once the stack is ready and `.bss` is cleared.
#[unsafe(no_mangle)]
pub extern "C" fn kernel_main(device_tree_address: usize) -> ! {
    install_exception_vectors();
    record_boot_ticks(read_counter_value());
    initialise_console_hardware();

    if !register_current_core(0) {
        // The device tree has not been read, so the power interface is not known yet.
        fatal!(None, "Fatal: unable to register the current core: 0");
    }

    // Every core needs a real frame to resume when the run queue is empty.
    install_idle_frames();
    log_banner();

    let device_tree = match load_device_tree(device_tree_address) {
        Ok(device_tree) => device_tree,
        Err(error) => fatal!(None, "Fatal: device tree unusable: {error:?}"),
    };
    let summary = device_tree.summary;
    let conduit = summary.power_state_coordination_interface_conduit;

    // Discovery is diagnostic in Phase 2; the memory allocator and boot MMU are unchanged.
    match &device_tree.hardware_description {
        Ok(hardware) => report_hardware_description(hardware),
        Err(error) => warning!(
            EarlyConsole,
            "Hardware discovery unavailable: {error:?}; refusing physical memory allocation",
        ),
    }

    // Firmware discovery is mandatory: allocating on an unknown map is unsafe.
    let hardware = match device_tree.hardware_description {
        Ok(hardware) => hardware,
        Err(error) => fatal!(conduit, "Fatal: hardware discovery failed: {error:?}"),
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

    // From here on the kernel allocates memory through the shared allocator.
    install_frame_allocator(frame_allocator);

    match with_frame_allocator(run_user_mapping_self_test) {
        Some(Ok(())) => information!(EarlyConsole, "User-mapping self-test passed"),
        Some(Err(reason)) => fatal!(conduit, "User-mapping self-test failed: {reason}"),
        None => error!(
            EarlyConsole,
            "User-mapping self-test skipped: frame allocator not installed"
        ),
    }

    initialise_interrupt_stack_tracking();
    initialise_reclamation_stack_tracking();
    verify_reclamation_stacks();

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
    verify_inbox_budget();

    information!(
        EarlyConsole,
        "Kernel bootstrap sequence complete. Handing control over to the scheduler..."
    );

    after_bootstrap(conduit)
}
