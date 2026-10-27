//! Physical memory, address spaces, and page tables.

pub mod address_space;
pub mod address_translation;
pub mod device_tree_blob;
pub mod frame_allocator;
pub mod kernel_image;
pub mod kernel_map;
pub mod kernel_stack;
pub mod page_tables;
pub mod private_page;
pub mod user_memory;
