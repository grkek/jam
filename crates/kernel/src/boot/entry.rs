//! First Rust code executed on the boot core.

use core::fmt::Write;

use foundation::{
    configuration::Configuration, device_tree::PowerStateCoordinationInterfaceConduit,
};

use super::{device_tree::load_device_tree, secondary_cores::start_secondary_cores};
use crate::{
    actors::table::spawn_user_actor,
    architecture::aarch64::{
        early_console::EarlyConsole, exceptions::install_exception_vectors,
        interrupt_controller::initialise_distributor, power_interface, processor,
        time::read_counter_value,
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
    scheduling::{
        core_local::register_current_core, idle::install_idle_frames,
        scheduler::start_current_core_tick,
    },
    warning,
};

const BYTES_PER_MEGABYTE: u64 = 1024 * 1024;
const SELF_TEST_FRAME_COUNT: usize = 2;

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
fn run_frame_allocator_self_test(boot_ticks: u64, frame_allocator: &mut FrameAllocator) {
    information!(
        EarlyConsole,
        boot_ticks,
        "Running self-test: verifying frame allocation and deallocation integrity..."
    );

    let initial_free_count = frame_allocator.free_frame_count();
    let first_frame = frame_allocator.allocate_frame();
    let second_frame = frame_allocator.allocate_frame();

    let (Some(first_frame), Some(second_frame)) = (first_frame, second_frame) else {
        error!(
            EarlyConsole,
            boot_ticks, "Self-test failed: unable to allocate test frames"
        );
        return;
    };

    information!(
        EarlyConsole,
        boot_ticks,
        "Self-test allocation passed: acquired frames at {first_frame:#x} and {second_frame:#x}"
    );

    if frame_allocator.free_frame_count()
        != initial_free_count.saturating_sub(SELF_TEST_FRAME_COUNT)
    {
        warning!(
            EarlyConsole,
            boot_ticks,
            "Self-test warning: frame count mismatch after allocation"
        );
    }

    let _ = frame_allocator.free_frame(first_frame);
    let _ = frame_allocator.free_frame(second_frame);

    if frame_allocator.free_frame_count() == initial_free_count {
        information!(
            EarlyConsole,
            boot_ticks,
            "Self-test deallocation passed: all test frames successfully returned to pool"
        );
    } else {
        error!(
            EarlyConsole,
            boot_ticks, "Self-test error: frame count did not match after release"
        );
    }
}

/// Called by the assembly entry once the stack is ready and `.bss` is cleared.
#[unsafe(no_mangle)]
pub extern "C" fn kernel_main(device_tree_address: usize) -> ! {
    install_exception_vectors();

    let boot_ticks = read_counter_value();

    if !register_current_core(0) {
        error!(
            EarlyConsole,
            boot_ticks, "Fatal: unable to register the current core: 0"
        );

        // The device tree has not been read, so the power interface is not known yet.
        power_off_or_park(None);
    }

    // Every core needs a real frame to resume when the run queue is empty.
    install_idle_frames();

    information!(
        EarlyConsole,
        boot_ticks,
        concat!(
            "JAM a.k.a Just Another Microkernel version ",
            env!("CARGO_PKG_VERSION"),
            " (aarch64-unknown-none) #1 SMP"
        )
    );

    information!(
        EarlyConsole,
        boot_ticks,
        "Exception vector table installed successfully"
    );

    information!(
        EarlyConsole,
        boot_ticks,
        "Kernel executing in the upper half, kernel_main at {:#x}",
        kernel_main as usize
    );

    let device_tree = match load_device_tree(device_tree_address) {
        Ok(device_tree) => device_tree,
        Err(error) => {
            error!(
                EarlyConsole,
                boot_ticks, "Fatal: device tree unusable: {error:?}"
            );

            power_off_or_park(None);
        }
    };
    let summary = device_tree.summary;
    let conduit = summary.power_state_coordination_interface_conduit;

    power_interface::record_conduit(conduit);

    information!(
        EarlyConsole,
        boot_ticks,
        "Device tree blob discovered at physical address {:#x}",
        device_tree.blob_address
    );

    let memory_end = summary.memory_base_address + summary.memory_byte_count;

    information!(
        EarlyConsole,
        boot_ticks,
        "Physical memory map: {:#x} to {:#x}, {} bytes ({} megabytes)",
        summary.memory_base_address,
        memory_end,
        summary.memory_byte_count,
        summary.memory_byte_count / BYTES_PER_MEGABYTE
    );

    information!(
        EarlyConsole,
        boot_ticks,
        "Kernel image occupies {:#x} to {:#x}",
        kernel_image_start_address(),
        kernel_image_end_address()
    );

    information!(
        EarlyConsole,
        boot_ticks,
        "Multiprocessing: {} execution cores detected; power state coordination interface: {:?}",
        summary.central_processing_unit_count,
        conduit
    );

    let Some(mut frame_allocator) = initialise_frame_allocator(&summary) else {
        error!(
            EarlyConsole,
            boot_ticks, "Fatal: physical frame allocator initialization failed"
        );

        power_off_or_park(conduit);
    };

    information!(
        EarlyConsole,
        boot_ticks,
        "Frame allocator initialized: {} of {} physical frames available",
        frame_allocator.free_frame_count(),
        frame_allocator.total_frame_count()
    );

    let kernel_address_space = match enable_kernel_address_space(&summary, &mut frame_allocator) {
        Ok(kernel_address_space) => kernel_address_space,
        Err(error) => {
            error!(
                EarlyConsole,
                boot_ticks, "Fatal: kernel address space unusable: {error:?}"
            );

            power_off_or_park(conduit);
        }
    };

    information!(
        EarlyConsole,
        boot_ticks,
        "Memory management unit enabled: translation tables at {:#x}, caches on, write-xor-execute enforced",
        kernel_address_space.root_table_address()
    );

    run_frame_allocator_self_test(boot_ticks, &mut frame_allocator);

    // From here on the kernel allocates memory through the shared allocator.
    install_frame_allocator(frame_allocator);

    match with_frame_allocator(run_user_mapping_self_test) {
        Some(Ok(())) => information!(EarlyConsole, boot_ticks, "User-mapping self-test passed"),
        Some(Err(reason)) => {
            error!(
                EarlyConsole,
                boot_ticks, "User-mapping self-test failed: {reason}"
            );

            power_off_or_park(conduit);
        }
        None => error!(
            EarlyConsole,
            boot_ticks, "User-mapping self-test skipped: frame allocator not installed"
        ),
    }

    initialise_distributor();

    // Secondary cores and the timer must be live before an actor can finish and land in idle.
    start_secondary_cores(
        &mut EarlyConsole,
        boot_ticks,
        &summary,
        kernel_address_space.root_table_address(),
    );

    start_current_core_tick();

    match spawn_user_actor(EL0_GENESIS_PROGRAM, None, Configuration::default()) {
        Ok(address) => information!(
            EarlyConsole,
            boot_ticks,
            "Genesis actor spawned successfully at address {address}"
        ),
        Err(spawn_error) => {
            error!(
                EarlyConsole,
                boot_ticks, "Fatal: failed to spawn Genesis actor: {spawn_error:?}"
            );
            power_off_or_park(conduit);
        }
    }
    
    information!(
        EarlyConsole,
        boot_ticks,
        "Kernel bootstrap sequence complete. Handing control over to the scheduler..."
    );

    loop {
        processor::wait_for_event();
    }
}
