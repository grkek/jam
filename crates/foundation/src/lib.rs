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
pub mod device_tree;
pub mod farewell;
pub mod frame_bitmap;
pub mod message_layout;
pub mod node_identifier;
pub mod page_table_entry;
pub mod resource_budget;
pub mod scroll;
pub mod system_call;
pub mod system_call_status;
mod wire_bytes;
