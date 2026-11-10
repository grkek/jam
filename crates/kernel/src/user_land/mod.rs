//! This module contains the user-land code that is linked into the kernel binary. It is responsible for starting the user-land actors, including the console driver, terminal, filesystem, etc.

pub mod console;

/// User land selected for this kernel binary.
pub use console as current;
