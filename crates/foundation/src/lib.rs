//! Shared vocabulary for the kernel, the runtime, and every service.
//!
//! Nothing in this crate touches hardware, so all of it is testable on the host.

#![no_std]
#![forbid(unsafe_code)]

#[cfg(test)]
extern crate std;

pub mod address;
pub mod autopsy;
pub mod cause;
pub mod configuration;
pub mod console;
pub mod device_tree;
pub mod farewell;
pub mod filesystem;
pub mod frame_bitmap;
/// Bounded physical memory, reservation, and CPU discovery from firmware.
pub mod hardware_description;
pub mod message_layout;
pub mod node_identifier;
pub mod page_table_entry;
/// Physical frame ownership and reserved-memory planning for early bootstrap.
pub mod physical_memory;
pub mod resource_budget;
pub mod scroll;
pub mod stress;
pub mod system_call;
pub mod system_call_status;
pub mod tag;
mod wire_bytes;

/// Register and record conventions for local actor messaging.
pub mod messaging;

pub mod observation;

const _: () = assert!(console::PROTOCOL != filesystem::PROTOCOL);
