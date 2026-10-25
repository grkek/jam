//! First Rust code executed on the boot core.

use core::fmt::Write;

use super::device_tree::load_device_tree_summary;
use crate::{
    architecture::aarch64::{
        early_console::EarlyConsole, exceptions::install_exception_vectors, time,
    },
    error,
    information,
    warning,
    memory::frame_allocator::initialise_frame_allocator,
};

/// Called by the assembly entry once the stack is ready and `.bss` is cleared.
#[unsafe(no_mangle)]
pub extern "C" fn kernel_main(device_tree_address: usize) -> ! {
    install_exception_vectors();

    let boot_ticks = time::read_counter_value();
    let mut console = EarlyConsole;

    information!(console, boot_ticks, "Exception vector table installed successfully");
    information!(console, boot_ticks, concat!("JAM a.k.a Just Another Microkernel version ", env!("CARGO_PKG_VERSION"), " (aarch64-unknown-none) #1 SMP"));
    information!(console, boot_ticks, "Device tree blob discovered at physical address {device_tree_address:#x}");

    let summary = match load_device_tree_summary(device_tree_address) {
        Ok(summary) => summary,
        Err(error) => {
            error!(console, boot_ticks, "Fatal: device tree unusable: {error:?}");
            halt()
        }
    };

    // Displaying memory map and kernel placement boundaries
    let memory_end = summary.memory_base_address + summary.memory_byte_count;

    information!(console, boot_ticks, "Physical memory map: usable region start {:#x}", summary.memory_base_address);
    information!(console, boot_ticks, "Physical memory map: usable region end {:#x}", memory_end);
    information!(console, boot_ticks, "Physical memory map: {} bytes available for allocation ({} megabytes)", summary.memory_byte_count, summary.memory_byte_count  / (1024 * 1024));

    information!(console, boot_ticks, "Kernel image loaded and executing at physical address 0x40200000");

    information!(
        console, boot_ticks,
        "Multiprocessing initialized: {} execution cores detected; power state coordination interface: {:?}",
        summary.cpu_count,
        summary.psci_conduit
    );

    let mut frame_allocator = match initialise_frame_allocator(&summary) {
        Some(allocator) => allocator,
        None => {
            error!(console, boot_ticks, "Fatal: physical frame allocator initialization failed");
            halt()
        }
    };

    information!(
        console, boot_ticks,
        "Frame allocator initialized: {} of {} physical frames available",
        frame_allocator.free_frame_count(),
        frame_allocator.total_frame_count(),
    );

    information!(console, boot_ticks, "Running self-test: verifying frame allocation and deallocation integrity...");

    let initial_free_count = frame_allocator.free_frame_count();

    let first_frame = frame_allocator.allocate_frame();
    let second_frame = frame_allocator.allocate_frame();

    match (first_frame, second_frame) {
        (Some(f1), Some(f2)) => {
            information!(console, boot_ticks, "Self-test allocation passed: acquired frames at {f1:#x} and {f2:#x}");
            
            // Verify frame count dropped correctly
            let allocated_count = frame_allocator.free_frame_count();
            if allocated_count != initial_free_count - 2 {
                warning!(console, boot_ticks, "Self-test warning: frame count mismatch after allocation");
            }

            let _ = frame_allocator.free_frame(f1);
            let _ = frame_allocator.free_frame(f2);

            if frame_allocator.free_frame_count() == initial_free_count {
                information!(console, boot_ticks, "Self-test deallocation passed: all test frames successfully returned to pool");
            } else {
                error!(console, boot_ticks, "Self-test error: frame count did not match after release");
            }
        }
        _ => {
            error!(console, boot_ticks, "Self-test failed: unable to allocate test frames");
        }
    }

    information!(console, boot_ticks, "Done.");

    halt()
}

/// Parks the core forever.
pub fn halt() -> ! {
    loop {
        core::hint::spin_loop();
    }
}