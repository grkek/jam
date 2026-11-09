//! Entry, device tree parsing, and secondary core bring-up.

pub mod device_tree;
pub mod entry;
pub mod secondary_cores;

#[cfg(feature = "stress")]
pub mod stress;

#[cfg(not(feature = "stress"))]
pub mod console;
