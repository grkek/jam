//! This module contains the user-land code that is linked into the kernel binary. It is responsible for starting the user-land actors.

pub mod console;

/// User land selected for this kernel binary.
pub use console as current;
