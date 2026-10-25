//! First Rust code executed on the boot core.

use core::fmt::Write;

use super::device_tree::load_device_tree;
use super::secondary_cores::start_secondary_cores;
use crate::{
    architecture::aarch64::{
        early_console::EarlyConsole, exceptions::install_exception_vectors, time,
    },
    error, information,
    memory::{
        frame_allocator::initialise_frame_allocator,
        kernel_image::{kernel_image_end_address, kernel_image_start_address},
        kernel_map::enable_kernel_address_space,
    },
    warning,
};

const BYTES_PER_MEGABYTE: u64 = 1024 * 1024;
const SELF_TEST_FRAME_COUNT: usize = 2;

/// Called by the assembly entry once the stack is ready and `.bss` is cleared.
#[unsafe(no_mangle)]
pub extern "C" fn kernel_main(device_tree_address: usize) -> ! {
    install_exception_vectors();

    let boot_ticks = time::read_counter_value();
    let mut console = EarlyConsole;

    information!(
        console,
        boot_ticks,
        concat!(
            "JAM a.k.a Just Another Microkernel version ",
            env!("CARGO_PKG_VERSION"),
            " (aarch64-unknown-none) #1 SMP"
        )
    );
    information!(
        console,
        boot_ticks,
        "Exception vector table installed successfully"
    );

    let device_tree = match load_device_tree(device_tree_address) {
        Ok(device_tree) => device_tree,
        Err(error) => {
            error!(
                console,
                boot_ticks, "Fatal: device tree unusable: {error:?}"
            );
            halt()
        }
    };
    let summary = device_tree.summary;
    information!(
        console,
        boot_ticks,
        "Device tree blob discovered at physical address {:#x}",
        device_tree.blob_address
    );

    let memory_end = summary.memory_base_address + summary.memory_byte_count;

    information!(
        console,
        boot_ticks,
        "Physical memory map: {:#x} to {:#x}, {} bytes ({} megabytes)",
        summary.memory_base_address,
        memory_end,
        summary.memory_byte_count,
        summary.memory_byte_count / BYTES_PER_MEGABYTE
    );

    information!(
        console,
        boot_ticks,
        "Kernel image occupies {:#x} to {:#x}",
        kernel_image_start_address(),
        kernel_image_end_address()
    );

    information!(
        console,
        boot_ticks,
        "Multiprocessing: {} execution cores detected; power state coordination interface: {:?}",
        summary.central_processing_unit_count,
        summary.power_state_coordination_interface_conduit
    );

    let Some(mut frame_allocator) = initialise_frame_allocator(&summary) else {
        error!(
            console,
            boot_ticks, "Fatal: physical frame allocator initialization failed"
        );
        halt()
    };

    information!(
        console,
        boot_ticks,
        "Frame allocator initialized: {} of {} physical frames available",
        frame_allocator.free_frame_count(),
        frame_allocator.total_frame_count()
    );

    let kernel_address_space = match enable_kernel_address_space(&summary, &mut frame_allocator) {
        Ok(kernel_address_space) => kernel_address_space,
        Err(error) => {
            error!(
                console,
                boot_ticks, "Fatal: kernel address space unusable: {error:?}"
            );
            halt()
        }
    };

    information!(
        console,
        boot_ticks,
        "Memory management unit enabled: translation tables at {:#x}, caches on, write-xor-execute enforced",
        kernel_address_space.root_table_address()
    );

    run_frame_allocator_self_test(&mut console, boot_ticks, &mut frame_allocator);

    start_secondary_cores(
        &mut console,
        boot_ticks,
        &summary,
        kernel_address_space.root_table_address(),
    );

    information!(console, boot_ticks, "Done.");

    halt()
}

/// Allocates and releases frames to prove the allocator still works with the MMU on.
fn run_frame_allocator_self_test(
    console: &mut EarlyConsole,
    boot_ticks: u64,
    frame_allocator: &mut crate::memory::frame_allocator::FrameAllocator,
) {
    information!(
        console,
        boot_ticks,
        "Running self-test: verifying frame allocation and deallocation integrity..."
    );

    let initial_free_count = frame_allocator.free_frame_count();
    let first_frame = frame_allocator.allocate_frame();
    let second_frame = frame_allocator.allocate_frame();

    let (Some(first_frame), Some(second_frame)) = (first_frame, second_frame) else {
        error!(
            console,
            boot_ticks, "Self-test failed: unable to allocate test frames"
        );
        return;
    };

    information!(
        console,
        boot_ticks,
        "Self-test allocation passed: acquired frames at {first_frame:#x} and {second_frame:#x}"
    );

    if frame_allocator.free_frame_count()
        != initial_free_count.saturating_sub(SELF_TEST_FRAME_COUNT)
    {
        warning!(
            console,
            boot_ticks,
            "Self-test warning: frame count mismatch after allocation"
        );
    }

    let _ = frame_allocator.free_frame(first_frame);
    let _ = frame_allocator.free_frame(second_frame);

    if frame_allocator.free_frame_count() == initial_free_count {
        information!(
            console,
            boot_ticks,
            "Self-test deallocation passed: all test frames successfully returned to pool"
        );
    } else {
        error!(
            console,
            boot_ticks, "Self-test error: frame count did not match after release"
        );
    }
}

/// Parks the core forever.
pub fn halt() -> ! {
    loop {
        core::hint::spin_loop();
    }
}
