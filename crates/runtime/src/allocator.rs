//! Kernel-backed pages owned by the calling actor.
//!
//! This initial interface exposes one contiguous allocation per actor.
//! Page counts are measured in 4 KiB frames. It is not a general-purpose heap.

use foundation::{system_call::SystemCall, system_call_status::SystemCallStatus};

use crate::system_call_wrappers::invoke;

/// An allocation belonging to the actor that requested it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Allocation {
    start_address: usize,
    page_count: usize,
}

impl Allocation {
    /// First mapped byte in the caller's private address space.
    #[must_use]
    pub const fn start_address(self) -> usize {
        self.start_address
    }

    /// Number of 4 KiB frames in this allocation.
    #[must_use]
    pub const fn page_count(self) -> usize {
        self.page_count
    }
}

fn check_status(raw_status: u64) -> Result<(), SystemCallStatus> {
    let status =
        SystemCallStatus::try_from(raw_status).unwrap_or(SystemCallStatus::UnknownSystemCall);
    if status == SystemCallStatus::Accepted {
        Ok(())
    } else {
        Err(status)
    }
}

/// Allocates zero-initialised, writable, non-executable pages.
///
/// # Errors
/// Returns kernel rejection when no free memory, budget, or allocation slot is available.
pub fn allocate_pages(page_count: usize) -> Result<Allocation, SystemCallStatus> {
    if page_count == 0 {
        return Err(SystemCallStatus::InvalidArgument);
    }
    // SAFETY: This syscall passes scalar arguments and returns the start address of
    // pages that the kernel exclusively maps into this actor's own address space.
    let (status, start_address) = unsafe {
        invoke(
            SystemCall::AllocatePages.number(),
            [page_count as u64, 0, 0, 0, 0, 0],
        )
    };
    check_status(status)?;
    let start_address =
        usize::try_from(start_address).map_err(|_| SystemCallStatus::InvalidArgument)?;
    if start_address == 0 || start_address % 4096 != 0 {
        return Err(SystemCallStatus::InvalidArgument);
    }
    Ok(Allocation {
        start_address,
        page_count,
    })
}

/// Releases exactly the pages associated with a prior allocation.
///
/// # Errors
/// Fails when the allocation is not held by this actor or the kernel rejects the release.
pub fn release_pages(allocation: Allocation) -> Result<(), SystemCallStatus> {
    // SAFETY: The arguments identify an allocation made by this actor. The
    // kernel validates that the exact region is still owned before unmapping it.
    let (status, _) = unsafe {
        invoke(
            SystemCall::ReleasePages.number(),
            [
                allocation.start_address as u64,
                allocation.page_count as u64,
                0,
                0,
                0,
                0,
            ],
        )
    };
    check_status(status)
}
