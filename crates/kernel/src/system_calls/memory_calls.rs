//! Dynamic EL0 page allocation and release for the calling actor.
//!
//! Initial ABI: AllocatePages takes x0=page count and returns the first
//! user virtual address in x1. ReleasePages takes x0=that address and
//! x1=the exact page count. A caller may own one such allocation at a time.
//!
//! Physical pages are zero-initialised and mapped user-writable, never
//! executable. The kernel owns the page tables and enforces the actor's
//! configured budget for this dynamic region. Pages are reclaimed on
//! explicit release or destruction of the actor's address space.

use core::ptr::write_bytes;

use foundation::{
    page_table_entry::{PAGE_BYTE_COUNT, PageAttributes, UserAccess},
    system_call::{STATUS_REGISTER, VALUE_REGISTER},
    system_call_status::SystemCallStatus,
};

use crate::{
    actors::{control_block::ActorState, table::with_control_block},
    architecture::aarch64::{
        exceptions::TrapFrame, memory_management_unit::invalidate_address_space_identifier,
    },
    memory::{
        address_space::AddressSpace,
        address_translation::physical_to_virtual,
        frame_allocator::{FrameAllocator, with_frame_allocator},
    },
    scheduling::core_local::{NO_ACTOR, current_core_local},
};

/// Reserved lower-half range for an actor's first dynamically allocated region.
const DYNAMIC_MEMORY_START_ADDRESS: usize = 0x0000_0040_0000_0000;
/// The initial implementation supports at most 1 MiB per actor.
const MAXIMUM_DYNAMIC_PAGE_COUNT: usize = 256;

type CallResult = Result<usize, SystemCallStatus>;

fn current_actor_slot() -> Result<usize, SystemCallStatus> {
    let slot = current_core_local()
        .ok_or(SystemCallStatus::InvalidArgument)?
        .current_slot_index();
    if slot == NO_ACTOR {
        Err(SystemCallStatus::InvalidArgument)
    } else {
        Ok(slot)
    }
}

/// Invalidate old mappings on every core before returning their frames to the allocator.
fn unmap_and_reclaim(
    address_space: &mut AddressSpace,
    frame_allocator: &mut FrameAllocator,
    address_space_identifier: u16,
    page_count: usize,
) {
    for page_index in 0..page_count {
        let address = DYNAMIC_MEMORY_START_ADDRESS + page_index * PAGE_BYTE_COUNT;
        let frame = address_space
            .unmap_page(address)
            .expect("Dynamic allocation lost a page-table entry");

        // SAFETY: An AArch64 inner-shareable barrier publishes the PTE
        // invalidation before the ASID-wide TLB invalidation on all cores.
        unsafe { core::arch::asm!("dsb ishst", options(nostack, preserves_flags)) };
        invalidate_address_space_identifier(address_space_identifier);

        frame_allocator
            .free_frame(frame)
            .expect("Dynamic allocation released an unowned physical frame");
    }
}

fn allocate(page_count: usize) -> CallResult {
    if page_count == 0 {
        return Err(SystemCallStatus::InvalidArgument);
    }
    if page_count > MAXIMUM_DYNAMIC_PAGE_COUNT {
        return Err(SystemCallStatus::BudgetExhausted);
    }

    let slot = current_actor_slot()?;
    with_control_block(slot, |actor| {
        if actor.state != ActorState::Running {
            return Err(SystemCallStatus::InvalidArgument);
        }
        if actor.dynamic_memory_page_count != 0 {
            return Err(SystemCallStatus::InvalidArgument);
        }
        let budget = actor
            .configuration
            .as_ref()
            .ok_or(SystemCallStatus::InvalidArgument)?
            .resource_budget
            .memory_budget_in_pages as usize;
        if page_count > budget {
            return Err(SystemCallStatus::BudgetExhausted);
        }
        let address_space = actor
            .address_space
            .as_mut()
            .ok_or(SystemCallStatus::InvalidArgument)?;
        let identifier = actor.address_space_identifier;

        // This fixed arena is deliberately disjoint from current ELF images and stacks.
        // Reject it if a future loader or mapping happens to occupy the range.
        if (0..page_count).any(|page_index| {
            address_space
                .lookup_page(DYNAMIC_MEMORY_START_ADDRESS + page_index * PAGE_BYTE_COUNT)
                .is_some()
        }) {
            return Err(SystemCallStatus::InvalidArgument);
        }

        let allocation_result = with_frame_allocator(|frame_allocator| {
            let attributes = PageAttributes::user(UserAccess::ReadWrite, false)
                .expect("Writable non-executable pages have valid attributes");
            let mut mapped_page_count = 0;

            let result = (|| {
                for page_index in 0..page_count {
                    let frame = frame_allocator
                        .allocate_frame()
                        .ok_or(SystemCallStatus::BudgetExhausted)?;
                    let address = DYNAMIC_MEMORY_START_ADDRESS + page_index * PAGE_BYTE_COUNT;

                    // Zero before mapping so no stale physical contents are exposed to EL0.
                    // SAFETY: The allocator just gave this frame exclusively to the kernel,
                    // and the kernel has a writable physical direct mapping for all its bytes.
                    unsafe {
                        write_bytes(physical_to_virtual(frame) as *mut u8, 0, PAGE_BYTE_COUNT);
                    }
                    if address_space
                        .map_page(frame_allocator, address, frame, attributes)
                        .is_err()
                    {
                        frame_allocator
                            .free_frame(frame)
                            .expect("Newly allocated physical frame could not be returned");
                        return Err(SystemCallStatus::BudgetExhausted);
                    }
                    mapped_page_count += 1;
                }
                Ok(())
            })();

            if result.is_err() {
                unmap_and_reclaim(
                    address_space,
                    frame_allocator,
                    identifier,
                    mapped_page_count,
                );
            }
            result
        })
        .ok_or(SystemCallStatus::BudgetExhausted)?;

        allocation_result?;
        actor.dynamic_memory_page_count = page_count;
        Ok(DYNAMIC_MEMORY_START_ADDRESS)
    })
    .ok_or(SystemCallStatus::InvalidArgument)?
}

fn release(start_address: usize, page_count: usize) -> CallResult {
    let slot = current_actor_slot()?;
    with_control_block(slot, |actor| {
        if actor.state != ActorState::Running
            || start_address != DYNAMIC_MEMORY_START_ADDRESS
            || page_count == 0
            || actor.dynamic_memory_page_count != page_count
        {
            return Err(SystemCallStatus::InvalidArgument);
        }
        let address_space = actor
            .address_space
            .as_mut()
            .ok_or(SystemCallStatus::InvalidArgument)?;
        for page_index in 0..page_count {
            let address = DYNAMIC_MEMORY_START_ADDRESS + page_index * PAGE_BYTE_COUNT;
            if address_space.lookup_page(address).is_none() {
                // A page can only disappear here if a kernel invariant was violated.
                panic!("Dynamic allocation has a missing page");
            }
        }
        with_frame_allocator(|frame_allocator| {
            unmap_and_reclaim(
                address_space,
                frame_allocator,
                actor.address_space_identifier,
                page_count,
            );
        })
        .ok_or(SystemCallStatus::BudgetExhausted)?;
        actor.dynamic_memory_page_count = 0;
        Ok(0)
    })
    .ok_or(SystemCallStatus::InvalidArgument)?
}

fn finish(frame: &mut TrapFrame, result: CallResult) {
    let (status, value) = match result {
        Ok(value) => (SystemCallStatus::Accepted, value as u64),
        Err(status) => (status, 0),
    };
    frame.general_registers[usize::from(STATUS_REGISTER)] = status.number();
    frame.general_registers[usize::from(VALUE_REGISTER)] = value;
}

/// AllocatePages system-call handler.
pub fn allocate_pages(frame: &mut TrapFrame) {
    let page_count = usize::try_from(frame.general_registers[0]);
    let result = page_count
        .map_err(|_| SystemCallStatus::InvalidArgument)
        .and_then(allocate);
    finish(frame, result);
}

/// ReleasePages system-call handler.
pub fn release_pages(frame: &mut TrapFrame) {
    let start_address = usize::try_from(frame.general_registers[0]);
    let page_count = usize::try_from(frame.general_registers[1]);
    let result = start_address
        .and_then(|address| page_count.map(|count| (address, count)))
        .map_err(|_| SystemCallStatus::InvalidArgument)
        .and_then(|(address, count)| release(address, count));
    finish(frame, result);
}
