//! Reads the few facts the kernel needs from a flattened device tree.

use crate::wire_bytes::read_array;

/// The first four bytes of every device tree blob.
pub const DEVICE_TREE_MAGIC: u32 = 0xd00d_feed;
/// Bytes in a device tree header.
pub const DEVICE_TREE_HEADER_BYTE_COUNT: usize = 40;

const TOTAL_SIZE_OFFSET: usize = 4;
const STRUCTURE_OFFSET_OFFSET: usize = 8;
const STRINGS_OFFSET_OFFSET: usize = 12;
const STRINGS_SIZE_OFFSET: usize = 32;
const STRUCTURE_SIZE_OFFSET: usize = 36;

const TOKEN_BEGIN_NODE: u32 = 1;
const TOKEN_END_NODE: u32 = 2;
const TOKEN_PROPERTY: u32 = 3;
const TOKEN_NOP: u32 = 4;
const TOKEN_END: u32 = 9;
const TOKEN_BYTE_COUNT: usize = 4;
const CELL_BYTE_COUNT: usize = 4;

const MAXIMUM_TRACKED_DEPTH: usize = 4;
const ROOT_DEPTH: usize = 1;
const TOP_LEVEL_NODE_DEPTH: usize = 2;
const CPU_NODE_DEPTH: usize = 3;
const DEFAULT_ADDRESS_CELL_COUNT: usize = 2;
const DEFAULT_SIZE_CELL_COUNT: usize = 2;
const LARGEST_CELL_COUNT: usize = 2;

const EMPTY_NAME: &[u8] = &[];
const MEMORY_NODE_NAME_PREFIX: &[u8] = b"memory";
const POWER_STATE_COORDINATION_INTERFACE_NODE_NAME: &[u8] = b"psci";
const CPUS_NODE_NAME: &[u8] = b"cpus";
const CPU_NODE_NAME_PREFIX: &[u8] = b"cpu@";
const ADDRESS_CELLS_PROPERTY: &[u8] = b"#address-cells";
const SIZE_CELLS_PROPERTY: &[u8] = b"#size-cells";
const REGISTER_PROPERTY: &[u8] = b"reg";
const METHOD_PROPERTY: &[u8] = b"method";

/// How the kernel calls the firmware's power interface.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PowerStateCoordinationInterfaceConduit {
    /// Through the `hvc` instruction.
    HypervisorCall,
    /// Through the `smc` instruction.
    SecureMonitorCall,
}

/// What the kernel learns from the device tree at boot.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DeviceTreeSummary {
    /// Physical address where the first memory region begins.
    pub memory_base_address: u64,
    /// Size of the first memory region in bytes.
    pub memory_byte_count: u64,
    /// How to reach the power interface, when the tree describes one.
    pub power_state_coordination_interface_conduit: Option<PowerStateCoordinationInterfaceConduit>,
    /// Number of CPU cores the tree lists.
    pub central_processing_unit_count: usize,
}

/// Why a device tree could not be read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeviceTreeError {
    /// The blob does not start with the device tree magic number.
    BadMagic,
    /// The blob ends before something it promised.
    Truncated,
    /// The blob is internally inconsistent.
    Malformed,
    /// The tree describes no memory.
    MissingMemory,
}

fn read_word_at(bytes: &[u8], offset: usize) -> Option<u32> {
    read_array(bytes, offset).map(u32::from_be_bytes)
}

fn read_size_at(bytes: &[u8], offset: usize) -> Result<usize, DeviceTreeError> {
    let value = read_word_at(bytes, offset).ok_or(DeviceTreeError::Truncated)?;
    usize::try_from(value).map_err(|_| DeviceTreeError::Malformed)
}

fn read_text_at(bytes: &[u8], offset: usize) -> Option<&[u8]> {
    let tail = bytes.get(offset..)?;
    let length = tail.iter().position(|byte| *byte == 0)?;
    tail.get(..length)
}

fn align_to_token(offset: usize) -> usize {
    (offset + TOKEN_BYTE_COUNT - 1) & !(TOKEN_BYTE_COUNT - 1)
}

fn read_cells(bytes: &[u8], offset: usize, cell_count: usize) -> Option<u64> {
    if cell_count == 0 || cell_count > LARGEST_CELL_COUNT {
        return None;
    }
    let mut value = 0_u64;
    for cell_index in 0..cell_count {
        let cell = read_word_at(bytes, offset + cell_index * CELL_BYTE_COUNT)?;
        value = (value << 32) | u64::from(cell);
    }
    Some(value)
}

fn read_cell_count(data: &[u8]) -> Result<usize, DeviceTreeError> {
    read_word_at(data, 0)
        .and_then(|value| usize::try_from(value).ok())
        .ok_or(DeviceTreeError::Malformed)
}

fn parse_conduit(method: &[u8]) -> Option<PowerStateCoordinationInterfaceConduit> {
    match method.split(|byte| *byte == 0).next() {
        Some(b"hvc") => Some(PowerStateCoordinationInterfaceConduit::HypervisorCall),
        Some(b"smc") => Some(PowerStateCoordinationInterfaceConduit::SecureMonitorCall),
        _ => None,
    }
}

/// Reads memory, the power interface conduit, and the CPU count from a device tree blob.
///
/// # Errors
///
/// Fails when the blob is not a valid device tree or describes no memory.
pub fn parse_device_tree(blob: &[u8]) -> Result<DeviceTreeSummary, DeviceTreeError> {
    let magic = read_word_at(blob, 0).ok_or(DeviceTreeError::Truncated)?;
    if magic != DEVICE_TREE_MAGIC {
        return Err(DeviceTreeError::BadMagic);
    }
    let total_size = read_size_at(blob, TOTAL_SIZE_OFFSET)?;
    let blob = blob.get(..total_size).ok_or(DeviceTreeError::Truncated)?;

    let structure_offset = read_size_at(blob, STRUCTURE_OFFSET_OFFSET)?;
    let structure_size = read_size_at(blob, STRUCTURE_SIZE_OFFSET)?;
    let strings_offset = read_size_at(blob, STRINGS_OFFSET_OFFSET)?;
    let strings_size = read_size_at(blob, STRINGS_SIZE_OFFSET)?;

    let structure = blob
        .get(structure_offset..structure_offset + structure_size)
        .ok_or(DeviceTreeError::Truncated)?;
    let strings = blob
        .get(strings_offset..strings_offset + strings_size)
        .ok_or(DeviceTreeError::Truncated)?;

    walk_structure(structure, strings)
}

fn walk_structure(structure: &[u8], strings: &[u8]) -> Result<DeviceTreeSummary, DeviceTreeError> {
    let mut address_cell_count = DEFAULT_ADDRESS_CELL_COUNT;
    let mut size_cell_count = DEFAULT_SIZE_CELL_COUNT;
    let mut memory: Option<(u64, u64)> = None;
    let mut power_state_coordination_interface_conduit = None;
    let mut central_processing_unit_count = 0;

    let mut node_names = [EMPTY_NAME; MAXIMUM_TRACKED_DEPTH];
    let mut depth = 0_usize;
    let mut offset = 0_usize;

    loop {
        let token = read_word_at(structure, offset).ok_or(DeviceTreeError::Truncated)?;
        offset += TOKEN_BYTE_COUNT;

        match token {
            TOKEN_BEGIN_NODE => {
                let name = read_text_at(structure, offset).ok_or(DeviceTreeError::Malformed)?;
                offset = align_to_token(offset + name.len() + 1);
                depth += 1;
                if let Some(tracked_name) = node_names.get_mut(depth) {
                    *tracked_name = name;
                }
                let parent_name = node_names.get(TOP_LEVEL_NODE_DEPTH).copied();
                if depth == CPU_NODE_DEPTH
                    && parent_name == Some(CPUS_NODE_NAME)
                    && name.starts_with(CPU_NODE_NAME_PREFIX)
                {
                    central_processing_unit_count += 1;
                }
            }
            TOKEN_END_NODE => {
                depth = depth.checked_sub(1).ok_or(DeviceTreeError::Malformed)?;
            }
            TOKEN_PROPERTY => {
                let length = read_size_at(structure, offset)?;
                let name_offset = read_size_at(structure, offset + TOKEN_BYTE_COUNT)?;
                offset += 2 * TOKEN_BYTE_COUNT;
                let data = structure
                    .get(offset..offset + length)
                    .ok_or(DeviceTreeError::Truncated)?;
                offset = align_to_token(offset + length);
                let property_name =
                    read_text_at(strings, name_offset).ok_or(DeviceTreeError::Malformed)?;
                let node_name = node_names.get(depth).copied().unwrap_or(EMPTY_NAME);

                if depth == ROOT_DEPTH && property_name == ADDRESS_CELLS_PROPERTY {
                    address_cell_count = read_cell_count(data)?;
                } else if depth == ROOT_DEPTH && property_name == SIZE_CELLS_PROPERTY {
                    size_cell_count = read_cell_count(data)?;
                } else if depth == TOP_LEVEL_NODE_DEPTH
                    && node_name.starts_with(MEMORY_NODE_NAME_PREFIX)
                    && property_name == REGISTER_PROPERTY
                    && memory.is_none()
                {
                    let base = read_cells(data, 0, address_cell_count)
                        .ok_or(DeviceTreeError::Malformed)?;
                    let size =
                        read_cells(data, address_cell_count * CELL_BYTE_COUNT, size_cell_count)
                            .ok_or(DeviceTreeError::Malformed)?;
                    memory = Some((base, size));
                } else if depth == TOP_LEVEL_NODE_DEPTH
                    && node_name.starts_with(POWER_STATE_COORDINATION_INTERFACE_NODE_NAME)
                    && property_name == METHOD_PROPERTY
                {
                    power_state_coordination_interface_conduit = parse_conduit(data);
                }
            }
            TOKEN_NOP => {}
            TOKEN_END => break,
            _ => return Err(DeviceTreeError::Malformed),
        }
    }

    let (memory_base_address, memory_byte_count) = memory.ok_or(DeviceTreeError::MissingMemory)?;
    Ok(DeviceTreeSummary {
        memory_base_address,
        memory_byte_count,
        power_state_coordination_interface_conduit,
        central_processing_unit_count,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::vec::Vec;

    struct BlobBuilder {
        structure: Vec<u8>,
        strings: Vec<u8>,
    }

    impl BlobBuilder {
        fn new() -> Self {
            Self {
                structure: Vec::new(),
                strings: Vec::new(),
            }
        }

        fn push_word(&mut self, value: u32) {
            self.structure.extend_from_slice(&value.to_be_bytes());
        }

        fn pad(&mut self) {
            while self.structure.len() % TOKEN_BYTE_COUNT != 0 {
                self.structure.push(0);
            }
        }

        fn begin_node(&mut self, name: &str) {
            self.push_word(TOKEN_BEGIN_NODE);
            self.structure.extend_from_slice(name.as_bytes());
            self.structure.push(0);
            self.pad();
        }

        fn end_node(&mut self) {
            self.push_word(TOKEN_END_NODE);
        }

        fn property(&mut self, name: &str, data: &[u8]) {
            let name_offset = self.strings.len();
            self.strings.extend_from_slice(name.as_bytes());
            self.strings.push(0);
            self.push_word(TOKEN_PROPERTY);
            self.push_word(u32::try_from(data.len()).unwrap());
            self.push_word(u32::try_from(name_offset).unwrap());
            self.structure.extend_from_slice(data);
            self.pad();
        }

        fn finish(mut self) -> Vec<u8> {
            self.push_word(TOKEN_END);
            let header_size = u32::try_from(DEVICE_TREE_HEADER_BYTE_COUNT).unwrap();
            let structure_size = u32::try_from(self.structure.len()).unwrap();
            let strings_size = u32::try_from(self.strings.len()).unwrap();
            let header_words = [
                DEVICE_TREE_MAGIC,
                header_size + structure_size + strings_size,
                header_size,
                header_size + structure_size,
                header_size,
                17,
                16,
                0,
                strings_size,
                structure_size,
            ];
            let mut blob: Vec<u8> = header_words
                .iter()
                .flat_map(|word| word.to_be_bytes())
                .collect();
            blob.extend_from_slice(&self.structure);
            blob.extend_from_slice(&self.strings);
            blob
        }
    }

    fn words(values: &[u32]) -> Vec<u8> {
        values
            .iter()
            .flat_map(|value| value.to_be_bytes())
            .collect()
    }

    fn sample_blob(power_state_coordination_interface_method: &[u8]) -> Vec<u8> {
        let mut builder = BlobBuilder::new();
        builder.begin_node("");
        builder.property("#address-cells", &words(&[2]));
        builder.property("#size-cells", &words(&[2]));
        builder.begin_node("memory@40000000");
        builder.property("reg", &words(&[0, 0x4000_0000, 0, 0x2000_0000]));
        builder.end_node();
        builder.begin_node("psci");
        builder.property("method", power_state_coordination_interface_method);
        builder.end_node();
        builder.begin_node("cpus");
        builder.begin_node("cpu@0");
        builder.end_node();
        builder.begin_node("cpu@1");
        builder.end_node();
        builder.end_node();
        builder.end_node();
        builder.finish()
    }

    #[test]
    fn reads_memory_conduit_and_central_processing_unit_count() {
        let summary = parse_device_tree(&sample_blob(b"hvc\0")).unwrap();
        assert_eq!(summary.memory_base_address, 0x4000_0000);
        assert_eq!(summary.memory_byte_count, 0x2000_0000);
        assert_eq!(
            summary.power_state_coordination_interface_conduit,
            Some(PowerStateCoordinationInterfaceConduit::HypervisorCall)
        );
        assert_eq!(summary.central_processing_unit_count, 2);
    }

    #[test]
    fn reads_the_secure_monitor_conduit() {
        let summary = parse_device_tree(&sample_blob(b"smc\0")).unwrap();
        assert_eq!(
            summary.power_state_coordination_interface_conduit,
            Some(PowerStateCoordinationInterfaceConduit::SecureMonitorCall)
        );
    }

    #[test]
    fn wrong_magic_is_rejected() {
        let mut blob = sample_blob(b"hvc\0");
        blob[0] = 0;
        assert_eq!(parse_device_tree(&blob), Err(DeviceTreeError::BadMagic));
    }

    #[test]
    fn truncated_blob_is_rejected() {
        let blob = sample_blob(b"hvc\0");
        assert_eq!(
            parse_device_tree(&blob[..DEVICE_TREE_HEADER_BYTE_COUNT]),
            Err(DeviceTreeError::Truncated)
        );
    }

    #[test]
    fn tree_without_memory_is_rejected() {
        let mut builder = BlobBuilder::new();
        builder.begin_node("");
        builder.end_node();
        assert_eq!(
            parse_device_tree(&builder.finish()),
            Err(DeviceTreeError::MissingMemory)
        );
    }
}
