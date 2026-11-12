//! Volatile in-memory file system with 9P-style handles.
//!
//! The layout deliberately mirrors a disk file system, so a disk backend can replace the
//! storage without changing the protocol: nodes play the part of inodes, the block pool
//! plays the part of the device, and a file reaches its bytes only through block indices.
//!
//! Clients never name nodes. They hold handles, each bound to the client it was issued to
//! and carrying the rights that client may exercise. A walk or a create only produces
//! handles with the rights of their source, so authority never grows as a client moves
//! through the tree.
//!
//! Operations are atomic: a refused operation changes nothing, except `remove`, which
//! always releases the handle it was given.

use crate::{
    address::{Address, LocalAddress},
    file_system_error::FileSystemError,
    file_system_handle::FileSystemHandle,
};

/// Longest name of a single path component, in bytes.
pub const MAXIMUM_NAME_BYTE_COUNT: usize = 32;
/// Most components a single walk may traverse.
pub const MAXIMUM_WALK_COMPONENT_COUNT: usize = 16;
/// Bytes in one storage block.
pub const BLOCK_BYTE_COUNT: usize = 256;
/// Blocks in the storage pool.
pub const BLOCK_COUNT: usize = 64;
/// Blocks a single file can reference.
pub const DIRECT_BLOCK_COUNT: usize = 4;
/// Largest file, in bytes.
pub const MAXIMUM_FILE_BYTE_COUNT: usize = BLOCK_BYTE_COUNT * DIRECT_BLOCK_COUNT;
/// Nodes the file system can hold, including the root.
pub const NODE_CAPACITY: usize = 64;
/// Handles that can exist at once, across all clients.
pub const HANDLE_CAPACITY: usize = 32;

const _: () = assert!(
    NODE_CAPACITY < u16::MAX as usize,
    "Node indices must fit in a u16"
);
const _: () = assert!(
    BLOCK_COUNT < u16::MAX as usize,
    "Block indices must fit in a u16"
);
const _: () = assert!(
    MAXIMUM_NAME_BYTE_COUNT <= u8::MAX as usize,
    "Name lengths must fit in a u8"
);
const _: () = assert!(
    MAXIMUM_FILE_BYTE_COUNT <= u32::MAX as usize,
    "File sizes must fit in a u32"
);

/// The root directory always occupies the first node.
const ROOT_NODE_INDEX: u16 = 0;
/// Marks a node with no parent.
const NO_NODE: u16 = u16::MAX;
/// Marks a file block slot with no block behind it. Such a slot reads as zeros.
const NO_BLOCK: u16 = u16::MAX;

/// What a handle is allowed to do. Rights can be inherited but never gained.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AccessRights {
    read: bool,
    write: bool,
}

impl AccessRights {
    /// Neither reading nor writing. Such a handle can still walk and statistics.
    pub const NONE: Self = Self::new(false, false);
    /// Reading only.
    pub const READ_ONLY: Self = Self::new(true, false);
    /// Reading and writing.
    pub const READ_WRITE: Self = Self::new(true, true);

    /// Builds a set of rights.
    #[must_use]
    pub const fn new(read: bool, write: bool) -> Self {
        Self { read, write }
    }

    /// Whether the rights include reading.
    #[must_use]
    pub const fn allows_read(self) -> bool {
        self.read
    }

    /// Whether the rights include writing, creating, and removing.
    #[must_use]
    pub const fn allows_write(self) -> bool {
        self.write
    }

    /// Whether every right in `required` is also in `self`.
    #[must_use]
    pub const fn covers(self, required: Self) -> bool {
        (self.read || !required.read) && (self.write || !required.write)
    }
}

/// How an opened handle may be used.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OpenMode {
    /// Reads only.
    Read,
    /// Writes only.
    Write,
    /// Reads and writes.
    ReadWrite,
}

impl OpenMode {
    /// The rights a handle needs before it can be opened in this mode.
    const fn required_rights(self) -> AccessRights {
        match self {
            Self::Read => AccessRights::READ_ONLY,
            Self::Write => AccessRights::new(false, true),
            Self::ReadWrite => AccessRights::READ_WRITE,
        }
    }

    /// Whether the mode permits reading.
    const fn reads(self) -> bool {
        matches!(self, Self::Read | Self::ReadWrite)
    }

    /// Whether the mode permits writing.
    const fn writes(self) -> bool {
        matches!(self, Self::Write | Self::ReadWrite)
    }
}

/// Whether a node holds children or bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NodeKind {
    /// A directory of named children.
    Directory,
    /// A file of bytes.
    File,
}

/// What `statistics` reports about a node.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NodeStatus {
    /// Directory or file.
    pub kind: NodeKind,
    /// Size of a file in bytes. Zero for a directory.
    pub byte_count: u32,
}

/// One child returned while listing a directory.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DirectoryEntry<'file_system> {
    /// Cursor that continues the listing after this entry. Never zero.
    pub next_cursor: u16,
    /// Directory or file.
    pub kind: NodeKind,
    /// The child's name within its directory.
    pub name: &'file_system [u8],
}

/// Non-empty, ASCII, short enough, free of separators, and not `.` or `..`.
///
/// `..` is refused on purpose. A handle to a subdirectory must not be able to climb back
/// out of it, so clients keep their own stack of ancestors instead.
#[must_use]
pub fn is_valid_name(name: &[u8]) -> bool {
    !name.is_empty()
        && name.len() <= MAXIMUM_NAME_BYTE_COUNT
        && name != b"."
        && name != b".."
        && name
            .iter()
            .all(|byte| byte.is_ascii_graphic() && *byte != b'/')
}

/// One stored node: the part of a file system that a disk would call an inode, plus its name.
#[derive(Clone, Copy)]
struct Node {
    in_use: bool,
    /// Changes every time the slot is freed, so handles to a removed node go stale.
    generation_counter: u32,
    kind: NodeKind,
    parent_index: u16,
    name_length: u8,
    name: [u8; MAXIMUM_NAME_BYTE_COUNT],
    byte_count: u32,
    block_indices: [u16; DIRECT_BLOCK_COUNT],
}

impl Node {
    const EMPTY: Self = Self {
        in_use: false,
        generation_counter: 0,
        kind: NodeKind::File,
        parent_index: NO_NODE,
        name_length: 0,
        name: [0; MAXIMUM_NAME_BYTE_COUNT],
        byte_count: 0,
        block_indices: [NO_BLOCK; DIRECT_BLOCK_COUNT],
    };

    const ROOT: Self = Self {
        in_use: true,
        kind: NodeKind::Directory,
        ..Self::EMPTY
    };

    fn name(&self) -> &[u8] {
        self.name
            .get(..usize::from(self.name_length))
            .unwrap_or(&[])
    }
}

/// One entry of the handle table.
#[derive(Clone, Copy)]
struct HandleSlot {
    in_use: bool,
    /// Changes every time the slot is released, so old handles stop working.
    generation_counter: u32,
    owner: Address,
    node_index: u16,
    /// The node generation this handle was issued for.
    node_generation_counter: u32,
    rights: AccessRights,
    opened: Option<OpenMode>,
}

impl HandleSlot {
    const EMPTY: Self = Self {
        in_use: false,
        generation_counter: 0,
        owner: Address::Local(LocalAddress::new(0, 0)),
        node_index: ROOT_NODE_INDEX,
        node_generation_counter: 0,
        rights: AccessRights::NONE,
        opened: None,
    };
}

/// A handle that passed validation, and what it refers to.
#[derive(Clone, Copy)]
struct Resolved {
    slot_index: usize,
    node_index: u16,
    rights: AccessRights,
    opened: Option<OpenMode>,
}

/// The whole file system: nodes, handles, and the block pool.
pub struct FileSystem {
    nodes: [Node; NODE_CAPACITY],
    handles: [HandleSlot; HANDLE_CAPACITY],
    blocks: [[u8; BLOCK_BYTE_COUNT]; BLOCK_COUNT],
    block_in_use: [bool; BLOCK_COUNT],
}

impl Default for FileSystem {
    fn default() -> Self {
        Self::new()
    }
}

impl FileSystem {
    /// An empty file system holding only the root directory.
    #[must_use]
    pub const fn new() -> Self {
        let mut nodes = [Node::EMPTY; NODE_CAPACITY];
        nodes[0] = Node::ROOT;
        Self {
            nodes,
            handles: [HandleSlot::EMPTY; HANDLE_CAPACITY],
            blocks: [[0; BLOCK_BYTE_COUNT]; BLOCK_COUNT],
            block_in_use: [false; BLOCK_COUNT],
        }
    }

    /// Issues `owner` a handle to the root directory with the given rights.
    ///
    /// This is the only way a client obtains its first handle. Boot calls it once for the
    /// client it trusts.
    ///
    /// # Errors
    ///
    /// Fails when every handle is in use.
    pub fn attach(
        &mut self,
        owner: Address,
        rights: AccessRights,
    ) -> Result<FileSystemHandle, FileSystemError> {
        self.allocate_handle(owner, ROOT_NODE_INDEX, rights)
    }

    /// Follows `names` from the directory `handle` refers to and returns a new handle to
    /// where it ends up, with the same rights. An empty `names` clones the handle.
    ///
    /// The walk is all or nothing: if any component fails, no handle is issued.
    ///
    /// # Errors
    ///
    /// Fails when the handle is invalid or already open, when a name is invalid or missing,
    /// when a component other than the last is not a directory, when there are too many
    /// components, or when every handle is in use.
    pub fn walk(
        &mut self,
        owner: Address,
        handle: FileSystemHandle,
        names: &[&[u8]],
    ) -> Result<FileSystemHandle, FileSystemError> {
        let resolved = self.resolve(owner, handle)?;
        if resolved.opened.is_some() {
            return Err(FileSystemError::AlreadyOpen);
        }
        if names.len() > MAXIMUM_WALK_COMPONENT_COUNT {
            return Err(FileSystemError::TooManyComponents);
        }

        let mut current = resolved.node_index;
        for name in names {
            if !is_valid_name(name) {
                return Err(FileSystemError::InvalidName);
            }
            if self.node_kind(current)? != NodeKind::Directory {
                return Err(FileSystemError::NotADirectory);
            }
            current = self
                .find_child(current, name)
                .ok_or(FileSystemError::NotFound)?;
        }
        self.allocate_handle(owner, current, resolved.rights)
    }

    /// Prepares `handle` for reading or writing.
    ///
    /// Rights are checked here and nowhere else on the data path, so `read` and `write`
    /// only need to look at the mode. Directories open for reading only.
    ///
    /// # Errors
    ///
    /// Fails when the handle is invalid or already open, when its rights do not cover the
    /// mode, or when a directory is opened for writing.
    pub fn open(
        &mut self,
        owner: Address,
        handle: FileSystemHandle,
        mode: OpenMode,
    ) -> Result<(), FileSystemError> {
        let resolved = self.resolve(owner, handle)?;
        if resolved.opened.is_some() {
            return Err(FileSystemError::AlreadyOpen);
        }
        if !resolved.rights.covers(mode.required_rights()) {
            return Err(FileSystemError::PermissionDenied);
        }
        if self.node_kind(resolved.node_index)? == NodeKind::Directory && mode != OpenMode::Read {
            return Err(FileSystemError::IsADirectory);
        }
        let slot = self
            .handles
            .get_mut(resolved.slot_index)
            .ok_or(FileSystemError::Inconsistent)?;
        slot.opened = Some(mode);
        Ok(())
    }

    /// Creates a child of the directory `directory` refers to and returns an unopened handle
    /// to it, with the same rights. The directory handle stays valid.
    ///
    /// 9P instead turns the directory's own handle into the new file. Returning a separate
    /// handle avoids losing the directory handle and the open-on-create special case.
    ///
    /// # Errors
    ///
    /// Fails when the handle is invalid or lacks the write right, when the name is invalid
    /// or taken, when the node is not a directory, or when no node or handle is free.
    pub fn create(
        &mut self,
        owner: Address,
        directory: FileSystemHandle,
        name: &[u8],
        kind: NodeKind,
    ) -> Result<FileSystemHandle, FileSystemError> {
        let resolved = self.resolve(owner, directory)?;
        if !resolved.rights.allows_write() {
            return Err(FileSystemError::PermissionDenied);
        }
        if !is_valid_name(name) {
            return Err(FileSystemError::InvalidName);
        }
        if self.node_kind(resolved.node_index)? != NodeKind::Directory {
            return Err(FileSystemError::NotADirectory);
        }
        if self.find_child(resolved.node_index, name).is_some() {
            return Err(FileSystemError::AlreadyExists);
        }
        if !self.handles.iter().any(|slot| !slot.in_use) {
            return Err(FileSystemError::HandlesExhausted);
        }

        let new_index = self
            .nodes
            .iter()
            .position(|node| !node.in_use)
            .ok_or(FileSystemError::CapacityExhausted)?;
        let new_node_index = u16::try_from(new_index).map_err(|_| FileSystemError::Inconsistent)?;
        let slot = self
            .nodes
            .get_mut(new_index)
            .ok_or(FileSystemError::Inconsistent)?;

        let mut fresh = Node {
            in_use: true,
            generation_counter: slot.generation_counter,
            kind,
            parent_index: resolved.node_index,
            name_length: u8::try_from(name.len()).map_err(|_| FileSystemError::Inconsistent)?,
            ..Node::EMPTY
        };
        fresh
            .name
            .get_mut(..name.len())
            .ok_or(FileSystemError::Inconsistent)?
            .copy_from_slice(name);
        *slot = fresh;

        self.allocate_handle(owner, new_node_index, resolved.rights)
    }

    /// Copies up to `destination.len()` bytes of a file, starting at `offset`, and returns
    /// how many were copied. Zero means the offset is at or past the end of the file.
    ///
    /// The file keeps no cursor, so a repeated request returns the same bytes.
    ///
    /// # Errors
    ///
    /// Fails when the handle is invalid, not open, not open for reading, or refers to a
    /// directory.
    pub fn read(
        &self,
        owner: Address,
        handle: FileSystemHandle,
        offset: u64,
        destination: &mut [u8],
    ) -> Result<usize, FileSystemError> {
        let resolved = self.resolve(owner, handle)?;
        let mode = resolved.opened.ok_or(FileSystemError::NotOpen)?;
        if !mode.reads() {
            return Err(FileSystemError::PermissionDenied);
        }
        let node = self
            .node(resolved.node_index)
            .ok_or(FileSystemError::Inconsistent)?;
        if node.kind == NodeKind::Directory {
            return Err(FileSystemError::IsADirectory);
        }

        let byte_count =
            usize::try_from(node.byte_count).map_err(|_| FileSystemError::Inconsistent)?;
        let Ok(offset) = usize::try_from(offset) else {
            return Ok(0);
        };
        if offset >= byte_count {
            return Ok(0);
        }
        let count = destination.len().min(byte_count - offset);

        let mut copied = 0;
        while copied < count {
            let position = offset + copied;
            let block_slot = position / BLOCK_BYTE_COUNT;
            let within_block = position % BLOCK_BYTE_COUNT;
            let chunk = (BLOCK_BYTE_COUNT - within_block).min(count - copied);
            let target = destination
                .get_mut(copied..copied + chunk)
                .ok_or(FileSystemError::Inconsistent)?;

            match node
                .block_indices
                .get(block_slot)
                .copied()
                .filter(|block_index| *block_index != NO_BLOCK)
            {
                Some(block_index) => {
                    let block = self
                        .blocks
                        .get(usize::from(block_index))
                        .ok_or(FileSystemError::Inconsistent)?;
                    let source = block
                        .get(within_block..within_block + chunk)
                        .ok_or(FileSystemError::Inconsistent)?;
                    target.copy_from_slice(source);
                }
                None => target.fill(0),
            }
            copied += chunk;
        }
        Ok(count)
    }

    /// Writes all of `data` to a file at `offset` and returns how many bytes were written.
    ///
    /// Writing past the end grows the file, and any gap reads as zeros. The write is all or
    /// nothing, so it never returns a short count.
    ///
    /// # Errors
    ///
    /// Fails when the handle is invalid, not open, not open for writing, or refers to a
    /// directory, when the write would pass the largest file size, or when the block pool
    /// cannot supply the blocks the write needs.
    pub fn write(
        &mut self,
        owner: Address,
        handle: FileSystemHandle,
        offset: u64,
        data: &[u8],
    ) -> Result<usize, FileSystemError> {
        let resolved = self.resolve(owner, handle)?;
        let mode = resolved.opened.ok_or(FileSystemError::NotOpen)?;
        if !mode.writes() {
            return Err(FileSystemError::PermissionDenied);
        }
        let node_index = resolved.node_index;
        if self.node_kind(node_index)? == NodeKind::Directory {
            return Err(FileSystemError::IsADirectory);
        }
        if data.is_empty() {
            return Ok(0);
        }

        let offset = usize::try_from(offset).map_err(|_| FileSystemError::FileTooLarge)?;
        let end = offset
            .checked_add(data.len())
            .filter(|end| *end <= MAXIMUM_FILE_BYTE_COUNT)
            .ok_or(FileSystemError::FileTooLarge)?;

        // Reserve every block the write touches before changing anything.
        let first_block = offset / BLOCK_BYTE_COUNT;
        let last_block = (end - 1) / BLOCK_BYTE_COUNT;
        let missing = (first_block..=last_block)
            .filter(|block_slot| self.block_index_of(node_index, *block_slot).is_none())
            .count();
        let free = self.block_in_use.iter().filter(|used| !**used).count();
        if missing > free {
            return Err(FileSystemError::OutOfSpace);
        }
        for block_slot in first_block..=last_block {
            self.ensure_block(node_index, block_slot)?;
        }

        let mut written = 0;
        while written < data.len() {
            let position = offset + written;
            let block_slot = position / BLOCK_BYTE_COUNT;
            let within_block = position % BLOCK_BYTE_COUNT;
            let chunk = (BLOCK_BYTE_COUNT - within_block).min(data.len() - written);
            let block_index = self
                .block_index_of(node_index, block_slot)
                .ok_or(FileSystemError::Inconsistent)?;
            let target = self
                .blocks
                .get_mut(usize::from(block_index))
                .and_then(|block| block.get_mut(within_block..within_block + chunk))
                .ok_or(FileSystemError::Inconsistent)?;
            let source = data
                .get(written..written + chunk)
                .ok_or(FileSystemError::Inconsistent)?;
            target.copy_from_slice(source);
            written += chunk;
        }

        let new_byte_count = u32::try_from(end).map_err(|_| FileSystemError::FileTooLarge)?;
        let node = self
            .node_mut(node_index)
            .ok_or(FileSystemError::Inconsistent)?;
        node.byte_count = node.byte_count.max(new_byte_count);
        Ok(data.len())
    }

    /// Returns the first child of an open directory stored at or after `cursor`.
    ///
    /// Start with a cursor of zero and pass each entry's `next_cursor` back in. `None` ends
    /// the listing. Entries come in table order, so a child created in a freed slot behind
    /// the cursor is not seen by a listing already in progress.
    ///
    /// # Errors
    ///
    /// Fails when the handle is invalid, not open, or refers to a file.
    pub fn read_directory(
        &self,
        owner: Address,
        handle: FileSystemHandle,
        cursor: u16,
    ) -> Result<Option<DirectoryEntry<'_>>, FileSystemError> {
        let resolved = self.resolve(owner, handle)?;
        resolved.opened.ok_or(FileSystemError::NotOpen)?;
        if self.node_kind(resolved.node_index)? != NodeKind::Directory {
            return Err(FileSystemError::NotADirectory);
        }

        let parent_index = resolved.node_index;
        Ok(self
            .nodes
            .iter()
            .enumerate()
            .skip(usize::from(cursor))
            .find_map(|(index, node)| {
                if !node.in_use || node.parent_index != parent_index {
                    return None;
                }
                Some(DirectoryEntry {
                    next_cursor: u16::try_from(index + 1).ok()?,
                    kind: node.kind,
                    name: node.name(),
                })
            }))
    }

    /// Reports the kind and size of the node a handle refers to.
    ///
    /// # Errors
    ///
    /// Fails when the handle is invalid or stale.
    pub fn statistics(
        &self,
        owner: Address,
        handle: FileSystemHandle,
    ) -> Result<NodeStatus, FileSystemError> {
        let resolved = self.resolve(owner, handle)?;
        let node = self
            .node(resolved.node_index)
            .ok_or(FileSystemError::Inconsistent)?;
        Ok(NodeStatus {
            kind: node.kind,
            byte_count: node.byte_count,
        })
    }

    /// Removes the node a handle refers to, and always releases the handle.
    ///
    /// Other handles to the removed node go stale and report [`FileSystemError::StaleHandle`]
    /// until their owners release them. A directory must be empty, and the root cannot be
    /// removed. The handle needs the write right.
    ///
    /// # Errors
    ///
    /// Fails when the handle is invalid, when it is stale, lacks the write right, refers to
    /// the root, or refers to a directory with children. Everything except an invalid handle
    /// still releases it.
    pub fn remove(
        &mut self,
        owner: Address,
        handle: FileSystemHandle,
    ) -> Result<(), FileSystemError> {
        let slot_index = self.locate_handle(owner, handle)?;
        let outcome = self.remove_referenced_node(slot_index);
        self.release_handle(slot_index)?;
        outcome
    }

    /// Releases a handle. This works even when the node behind it has been removed.
    ///
    /// # Errors
    ///
    /// Fails when the handle is invalid.
    pub fn clunk(
        &mut self,
        owner: Address,
        handle: FileSystemHandle,
    ) -> Result<(), FileSystemError> {
        let slot_index = self.locate_handle(owner, handle)?;
        self.release_handle(slot_index)
    }

    /// Releases every handle `owner` holds and returns how many there were.
    ///
    /// Call this when a client actor dies, so its handles do not leak.
    pub fn release_owner(&mut self, owner: Address) -> usize {
        let mut released = 0;
        for slot in &mut self.handles {
            if slot.in_use && slot.owner == owner {
                slot.in_use = false;
                slot.opened = None;
                slot.generation_counter = slot.generation_counter.wrapping_add(1);
                released += 1;
            }
        }
        released
    }

    fn node(&self, node_index: u16) -> Option<&Node> {
        self.nodes.get(usize::from(node_index))
    }

    fn node_mut(&mut self, node_index: u16) -> Option<&mut Node> {
        self.nodes.get_mut(usize::from(node_index))
    }

    fn node_kind(&self, node_index: u16) -> Result<NodeKind, FileSystemError> {
        self.node(node_index)
            .map(|node| node.kind)
            .ok_or(FileSystemError::Inconsistent)
    }

    /// The child of `parent_index` with this name, if any.
    fn find_child(&self, parent_index: u16, name: &[u8]) -> Option<u16> {
        self.nodes.iter().enumerate().find_map(|(index, node)| {
            if node.in_use && node.parent_index == parent_index && node.name() == name {
                u16::try_from(index).ok()
            } else {
                None
            }
        })
    }

    fn has_children(&self, node_index: u16) -> bool {
        self.nodes
            .iter()
            .any(|node| node.in_use && node.parent_index == node_index)
    }

    /// Checks that the handle was issued, is current, and belongs to `owner`.
    fn locate_handle(
        &self,
        owner: Address,
        handle: FileSystemHandle,
    ) -> Result<usize, FileSystemError> {
        let slot_index =
            usize::try_from(handle.slot_index()).map_err(|_| FileSystemError::InvalidHandle)?;
        let slot = self
            .handles
            .get(slot_index)
            .ok_or(FileSystemError::InvalidHandle)?;
        if slot.in_use
            && slot.generation_counter == handle.generation_counter()
            && slot.owner == owner
        {
            Ok(slot_index)
        } else {
            Err(FileSystemError::InvalidHandle)
        }
    }

    /// Like `locate_handle`, and also checks that the node behind it still exists.
    fn resolve(
        &self,
        owner: Address,
        handle: FileSystemHandle,
    ) -> Result<Resolved, FileSystemError> {
        let slot_index = self.locate_handle(owner, handle)?;
        let slot = self
            .handles
            .get(slot_index)
            .ok_or(FileSystemError::Inconsistent)?;
        let node = self
            .node(slot.node_index)
            .ok_or(FileSystemError::Inconsistent)?;
        if !node.in_use || node.generation_counter != slot.node_generation_counter {
            return Err(FileSystemError::StaleHandle);
        }
        Ok(Resolved {
            slot_index,
            node_index: slot.node_index,
            rights: slot.rights,
            opened: slot.opened,
        })
    }

    fn allocate_handle(
        &mut self,
        owner: Address,
        node_index: u16,
        rights: AccessRights,
    ) -> Result<FileSystemHandle, FileSystemError> {
        let node_generation_counter = self
            .node(node_index)
            .map(|node| node.generation_counter)
            .ok_or(FileSystemError::Inconsistent)?;
        let slot_index = self
            .handles
            .iter()
            .position(|slot| !slot.in_use)
            .ok_or(FileSystemError::HandlesExhausted)?;
        let slot_number = u32::try_from(slot_index).map_err(|_| FileSystemError::Inconsistent)?;
        let slot = self
            .handles
            .get_mut(slot_index)
            .ok_or(FileSystemError::Inconsistent)?;

        slot.in_use = true;
        slot.owner = owner;
        slot.node_index = node_index;
        slot.node_generation_counter = node_generation_counter;
        slot.rights = rights;
        slot.opened = None;

        Ok(FileSystemHandle::new(slot_number, slot.generation_counter))
    }

    fn release_handle(&mut self, slot_index: usize) -> Result<(), FileSystemError> {
        let slot = self
            .handles
            .get_mut(slot_index)
            .ok_or(FileSystemError::Inconsistent)?;
        slot.in_use = false;
        slot.opened = None;
        slot.generation_counter = slot.generation_counter.wrapping_add(1);
        Ok(())
    }

    /// Removes the node behind the handle in `slot_index`. The handle itself stays allocated.
    fn remove_referenced_node(&mut self, slot_index: usize) -> Result<(), FileSystemError> {
        let slot = self
            .handles
            .get(slot_index)
            .ok_or(FileSystemError::Inconsistent)?;
        let node_index = slot.node_index;
        let node_generation_counter = slot.node_generation_counter;
        let rights = slot.rights;

        let node = self.node(node_index).ok_or(FileSystemError::Inconsistent)?;
        if !node.in_use || node.generation_counter != node_generation_counter {
            return Err(FileSystemError::StaleHandle);
        }
        if !rights.allows_write() {
            return Err(FileSystemError::PermissionDenied);
        }
        if node_index == ROOT_NODE_INDEX {
            return Err(FileSystemError::CannotRemoveRoot);
        }
        if node.kind == NodeKind::Directory && self.has_children(node_index) {
            return Err(FileSystemError::NotEmpty);
        }

        self.release_blocks(node_index)?;
        let node = self
            .node_mut(node_index)
            .ok_or(FileSystemError::Inconsistent)?;
        *node = Node {
            generation_counter: node.generation_counter.wrapping_add(1),
            ..Node::EMPTY
        };
        Ok(())
    }

    /// The block behind a file's block slot, if one has been allocated.
    fn block_index_of(&self, node_index: u16, block_slot: usize) -> Option<u16> {
        self.node(node_index)?
            .block_indices
            .get(block_slot)
            .copied()
            .filter(|block_index| *block_index != NO_BLOCK)
    }

    /// Gives a file's block slot a zeroed block if it has none yet.
    fn ensure_block(&mut self, node_index: u16, block_slot: usize) -> Result<(), FileSystemError> {
        if self.block_index_of(node_index, block_slot).is_some() {
            return Ok(());
        }
        let free = self
            .block_in_use
            .iter()
            .position(|used| !*used)
            .ok_or(FileSystemError::OutOfSpace)?;
        let block_index = u16::try_from(free).map_err(|_| FileSystemError::Inconsistent)?;

        *self
            .block_in_use
            .get_mut(free)
            .ok_or(FileSystemError::Inconsistent)? = true;
        self.blocks
            .get_mut(free)
            .ok_or(FileSystemError::Inconsistent)?
            .fill(0);
        *self
            .node_mut(node_index)
            .and_then(|node| node.block_indices.get_mut(block_slot))
            .ok_or(FileSystemError::Inconsistent)? = block_index;
        Ok(())
    }

    /// Returns every block a file holds to the pool.
    fn release_blocks(&mut self, node_index: u16) -> Result<(), FileSystemError> {
        let block_indices = self
            .node(node_index)
            .ok_or(FileSystemError::Inconsistent)?
            .block_indices;
        for block_index in block_indices
            .into_iter()
            .filter(|block_index| *block_index != NO_BLOCK)
        {
            *self
                .block_in_use
                .get_mut(usize::from(block_index))
                .ok_or(FileSystemError::Inconsistent)? = false;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn client() -> Address {
        Address::Local(LocalAddress::new(1, 1))
    }

    fn other_client() -> Address {
        Address::Local(LocalAddress::new(2, 1))
    }

    fn attached() -> (FileSystem, FileSystemHandle) {
        let mut file_system = FileSystem::new();
        let root = file_system
            .attach(client(), AccessRights::READ_WRITE)
            .expect("attach to a fresh file system");
        (file_system, root)
    }

    fn create_file(
        file_system: &mut FileSystem,
        parent: FileSystemHandle,
        name: &[u8],
    ) -> FileSystemHandle {
        file_system
            .create(client(), parent, name, NodeKind::File)
            .expect("create a file")
    }

    fn create_directory(
        file_system: &mut FileSystem,
        parent: FileSystemHandle,
        name: &[u8],
    ) -> FileSystemHandle {
        file_system
            .create(client(), parent, name, NodeKind::Directory)
            .expect("create a directory")
    }

    #[test]
    fn written_bytes_round_trip() {
        let (mut file_system, root) = attached();
        let file = create_file(&mut file_system, root, b"notes");
        file_system
            .open(client(), file, OpenMode::ReadWrite)
            .expect("open for reading and writing");
        assert_eq!(file_system.write(client(), file, 0, b"hello world"), Ok(11));

        let mut buffer = [0_u8; 32];
        assert_eq!(file_system.read(client(), file, 0, &mut buffer), Ok(11));
        assert_eq!(buffer.get(..11), Some(&b"hello world"[..]));

        assert_eq!(file_system.read(client(), file, 6, &mut buffer), Ok(5));
        assert_eq!(buffer.get(..5), Some(&b"world"[..]));
    }

    #[test]
    fn reading_at_or_past_the_end_returns_nothing() {
        let (mut file_system, root) = attached();
        let file = create_file(&mut file_system, root, b"short");
        file_system
            .open(client(), file, OpenMode::ReadWrite)
            .expect("open");
        file_system.write(client(), file, 0, b"abc").expect("write");

        let mut buffer = [0_u8; 8];
        assert_eq!(file_system.read(client(), file, 3, &mut buffer), Ok(0));
        assert_eq!(file_system.read(client(), file, 1_000, &mut buffer), Ok(0));
    }

    #[test]
    fn write_across_a_block_boundary_round_trips() {
        let (mut file_system, root) = attached();
        let file = create_file(&mut file_system, root, b"big");
        file_system
            .open(client(), file, OpenMode::ReadWrite)
            .expect("open");

        let data: [u8; 300] = core::array::from_fn(|index| u8::try_from(index % 251).unwrap_or(0));
        assert_eq!(file_system.write(client(), file, 200, &data), Ok(300));

        let mut buffer = [0_u8; 300];
        assert_eq!(file_system.read(client(), file, 200, &mut buffer), Ok(300));
        assert_eq!(buffer, data);
    }

    #[test]
    fn a_gap_before_a_write_reads_as_zeros() {
        let (mut file_system, root) = attached();
        let file = create_file(&mut file_system, root, b"sparse");
        file_system
            .open(client(), file, OpenMode::ReadWrite)
            .expect("open");
        assert_eq!(file_system.write(client(), file, 600, b"tail"), Ok(4));
        assert_eq!(
            file_system.statistics(client(), file),
            Ok(NodeStatus {
                kind: NodeKind::File,
                byte_count: 604
            })
        );

        let mut buffer = [0xFF_u8; 16];
        assert_eq!(file_system.read(client(), file, 0, &mut buffer), Ok(16));
        assert_eq!(buffer, [0_u8; 16]);
    }

    #[test]
    fn writes_past_the_largest_file_size_are_rejected() {
        let (mut file_system, root) = attached();
        let file = create_file(&mut file_system, root, b"limit");
        file_system
            .open(client(), file, OpenMode::ReadWrite)
            .expect("open");

        let oversized = [0_u8; MAXIMUM_FILE_BYTE_COUNT + 1];
        assert_eq!(
            file_system.write(client(), file, 0, &oversized),
            Err(FileSystemError::FileTooLarge)
        );

        let at_limit = [1_u8; MAXIMUM_FILE_BYTE_COUNT];
        assert_eq!(
            file_system.write(client(), file, 0, &at_limit),
            Ok(MAXIMUM_FILE_BYTE_COUNT)
        );
        let end = u64::try_from(MAXIMUM_FILE_BYTE_COUNT).expect("the limit fits in a u64");
        assert_eq!(
            file_system.write(client(), file, end, b"x"),
            Err(FileSystemError::FileTooLarge)
        );
    }

    #[test]
    fn invalid_names_are_rejected() {
        let (mut file_system, root) = attached();
        let long_name = [b'x'; MAXIMUM_NAME_BYTE_COUNT + 1];
        let names: [&[u8]; 6] = [b"", b".", b"..", b"a/b", b"has space", &long_name];

        for name in names {
            assert_eq!(
                file_system.create(client(), root, name, NodeKind::File),
                Err(FileSystemError::InvalidName)
            );
            assert_eq!(
                file_system.walk(client(), root, &[name]),
                Err(FileSystemError::InvalidName)
            );
        }
    }

    #[test]
    fn duplicate_names_are_rejected() {
        let (mut file_system, root) = attached();
        create_file(&mut file_system, root, b"once");
        assert_eq!(
            file_system.create(client(), root, b"once", NodeKind::Directory),
            Err(FileSystemError::AlreadyExists)
        );
    }

    #[test]
    fn walking_finds_a_nested_node() {
        let (mut file_system, root) = attached();
        let documents = create_directory(&mut file_system, root, b"documents");
        create_file(&mut file_system, documents, b"readme");

        let names: [&[u8]; 2] = [b"documents", b"readme"];
        let found = file_system
            .walk(client(), root, &names)
            .expect("walk to the nested file");
        assert_eq!(
            file_system.statistics(client(), found),
            Ok(NodeStatus {
                kind: NodeKind::File,
                byte_count: 0
            })
        );
    }

    #[test]
    fn walking_to_a_missing_name_fails_without_issuing_a_handle() {
        let (mut file_system, root) = attached();
        let documents = create_directory(&mut file_system, root, b"documents");
        file_system.clunk(client(), documents).expect("clunk");

        let names: [&[u8]; 2] = [b"documents", b"missing"];
        assert_eq!(
            file_system.walk(client(), root, &names),
            Err(FileSystemError::NotFound)
        );
        // Only the root handle exists, so the next handle takes the second slot.
        let next = file_system
            .attach(client(), AccessRights::NONE)
            .expect("attach");
        assert_eq!(next.slot_index(), 1);
    }

    #[test]
    fn walking_through_a_file_fails() {
        let (mut file_system, root) = attached();
        create_file(&mut file_system, root, b"plain");

        let names: [&[u8]; 2] = [b"plain", b"inside"];
        assert_eq!(
            file_system.walk(client(), root, &names),
            Err(FileSystemError::NotADirectory)
        );
    }

    #[test]
    fn too_many_components_are_rejected() {
        let (mut file_system, root) = attached();
        let names: [&[u8]; MAXIMUM_WALK_COMPONENT_COUNT + 1] =
            [b"a"; MAXIMUM_WALK_COMPONENT_COUNT + 1];
        assert_eq!(
            file_system.walk(client(), root, &names),
            Err(FileSystemError::TooManyComponents)
        );
    }

    #[test]
    fn an_empty_walk_clones_the_handle() {
        let (mut file_system, root) = attached();
        let names: [&[u8]; 0] = [];
        let clone = file_system.walk(client(), root, &names).expect("clone");

        assert_ne!(clone, root);
        assert_eq!(
            file_system.statistics(client(), clone),
            file_system.statistics(client(), root)
        );
    }

    #[test]
    fn another_client_cannot_use_a_handle() {
        let (file_system, root) = attached();
        assert_eq!(
            file_system.statistics(other_client(), root),
            Err(FileSystemError::InvalidHandle)
        );
    }

    #[test]
    fn a_released_handle_stays_dead_when_its_slot_is_reused() {
        let (mut file_system, root) = attached();
        let names: [&[u8]; 0] = [];
        let first = file_system.walk(client(), root, &names).expect("walk");
        file_system.clunk(client(), first).expect("clunk");
        assert_eq!(
            file_system.statistics(client(), first),
            Err(FileSystemError::InvalidHandle)
        );

        let second = file_system.walk(client(), root, &names).expect("walk");
        assert_eq!(second.slot_index(), first.slot_index());
        assert_ne!(second.generation_counter(), first.generation_counter());
        assert_eq!(
            file_system.statistics(client(), first),
            Err(FileSystemError::InvalidHandle)
        );
        assert!(file_system.statistics(client(), second).is_ok());
    }

    #[test]
    fn a_handle_must_be_opened_before_data_moves() {
        let (mut file_system, root) = attached();
        let file = create_file(&mut file_system, root, b"closed");

        let mut buffer = [0_u8; 4];
        assert_eq!(
            file_system.read(client(), file, 0, &mut buffer),
            Err(FileSystemError::NotOpen)
        );
        assert_eq!(
            file_system.write(client(), file, 0, b"data"),
            Err(FileSystemError::NotOpen)
        );
    }

    #[test]
    fn the_open_mode_limits_what_can_be_done() {
        let (mut file_system, root) = attached();
        let file = create_file(&mut file_system, root, b"modes");
        file_system
            .open(client(), file, OpenMode::Write)
            .expect("open");

        let mut buffer = [0_u8; 4];
        assert_eq!(
            file_system.read(client(), file, 0, &mut buffer),
            Err(FileSystemError::PermissionDenied)
        );

        let reader = create_file(&mut file_system, root, b"reader");
        file_system
            .open(client(), reader, OpenMode::Read)
            .expect("open");
        assert_eq!(
            file_system.write(client(), reader, 0, b"data"),
            Err(FileSystemError::PermissionDenied)
        );
    }

    #[test]
    fn rights_are_checked_when_opening_and_creating() {
        let mut file_system = FileSystem::new();
        let root = file_system
            .attach(client(), AccessRights::READ_ONLY)
            .expect("attach");

        assert_eq!(
            file_system.create(client(), root, b"denied", NodeKind::File),
            Err(FileSystemError::PermissionDenied)
        );
        assert_eq!(
            file_system.open(client(), root, OpenMode::Write),
            Err(FileSystemError::PermissionDenied)
        );
        assert_eq!(file_system.open(client(), root, OpenMode::Read), Ok(()));
    }

    #[test]
    fn handles_cannot_be_opened_twice_or_walked_once_open() {
        let (mut file_system, root) = attached();
        let file = create_file(&mut file_system, root, b"busy");
        file_system
            .open(client(), file, OpenMode::Read)
            .expect("open");

        let names: [&[u8]; 0] = [];
        assert_eq!(
            file_system.open(client(), file, OpenMode::Read),
            Err(FileSystemError::AlreadyOpen)
        );
        assert_eq!(
            file_system.walk(client(), file, &names),
            Err(FileSystemError::AlreadyOpen)
        );
    }

    #[test]
    fn directories_open_for_reading_only() {
        let (mut file_system, root) = attached();
        assert_eq!(
            file_system.open(client(), root, OpenMode::Write),
            Err(FileSystemError::IsADirectory)
        );
        assert_eq!(
            file_system.open(client(), root, OpenMode::ReadWrite),
            Err(FileSystemError::IsADirectory)
        );
        assert_eq!(file_system.open(client(), root, OpenMode::Read), Ok(()));
    }

    #[test]
    fn listing_returns_children_in_order_and_then_ends() {
        let (mut file_system, root) = attached();
        create_file(&mut file_system, root, b"a");
        create_directory(&mut file_system, root, b"b");
        create_file(&mut file_system, root, b"c");
        file_system
            .open(client(), root, OpenMode::Read)
            .expect("open");

        let first = file_system
            .read_directory(client(), root, 0)
            .expect("list")
            .expect("a first entry");
        assert_eq!(first.name, b"a");
        assert_eq!(first.kind, NodeKind::File);

        let second = file_system
            .read_directory(client(), root, first.next_cursor)
            .expect("list")
            .expect("a second entry");
        assert_eq!(second.name, b"b");
        assert_eq!(second.kind, NodeKind::Directory);

        let third = file_system
            .read_directory(client(), root, second.next_cursor)
            .expect("list")
            .expect("a third entry");
        assert_eq!(third.name, b"c");

        assert_eq!(
            file_system.read_directory(client(), root, third.next_cursor),
            Ok(None)
        );
    }

    #[test]
    fn listing_a_file_fails() {
        let (mut file_system, root) = attached();
        let file = create_file(&mut file_system, root, b"plain");
        file_system
            .open(client(), file, OpenMode::Read)
            .expect("open");
        assert_eq!(
            file_system.read_directory(client(), file, 0),
            Err(FileSystemError::NotADirectory)
        );
    }

    #[test]
    fn removing_a_node_makes_other_handles_to_it_stale() {
        let (mut file_system, root) = attached();
        let file = create_file(&mut file_system, root, b"temporary");
        let names: [&[u8]; 1] = [b"temporary"];
        let other = file_system.walk(client(), root, &names).expect("walk");

        assert_eq!(file_system.remove(client(), file), Ok(()));
        assert_eq!(
            file_system.statistics(client(), file),
            Err(FileSystemError::InvalidHandle)
        );
        assert_eq!(
            file_system.statistics(client(), other),
            Err(FileSystemError::StaleHandle)
        );

        // Reusing the node slot must not revive the old handle.
        create_file(&mut file_system, root, b"temporary");
        assert_eq!(
            file_system.statistics(client(), other),
            Err(FileSystemError::StaleHandle)
        );
        assert_eq!(file_system.clunk(client(), other), Ok(()));
    }

    #[test]
    fn a_directory_with_children_cannot_be_removed() {
        let (mut file_system, root) = attached();
        let directory = create_directory(&mut file_system, root, b"full");
        create_file(&mut file_system, directory, b"inside");

        assert_eq!(
            file_system.remove(client(), directory),
            Err(FileSystemError::NotEmpty)
        );
        // As in 9P, the handle is released even though the removal failed.
        assert_eq!(
            file_system.statistics(client(), directory),
            Err(FileSystemError::InvalidHandle)
        );
    }

    #[test]
    fn the_root_cannot_be_removed() {
        let (mut file_system, root) = attached();
        assert_eq!(
            file_system.remove(client(), root),
            Err(FileSystemError::CannotRemoveRoot)
        );
    }

    #[test]
    fn removing_a_file_returns_its_blocks_to_the_pool() {
        let (mut file_system, root) = attached();
        let full = [7_u8; MAXIMUM_FILE_BYTE_COUNT];

        for index in 0..BLOCK_COUNT / DIRECT_BLOCK_COUNT {
            let name = std::format!("f{index}");
            let file = create_file(&mut file_system, root, name.as_bytes());
            file_system
                .open(client(), file, OpenMode::Write)
                .expect("open");
            assert_eq!(
                file_system.write(client(), file, 0, &full),
                Ok(MAXIMUM_FILE_BYTE_COUNT)
            );
            file_system.clunk(client(), file).expect("clunk");
        }

        let overflow = create_file(&mut file_system, root, b"overflow");
        file_system
            .open(client(), overflow, OpenMode::Write)
            .expect("open");
        assert_eq!(
            file_system.write(client(), overflow, 0, &full),
            Err(FileSystemError::OutOfSpace)
        );

        let names: [&[u8]; 1] = [b"f0"];
        let first = file_system.walk(client(), root, &names).expect("walk");
        file_system.remove(client(), first).expect("remove");

        assert_eq!(
            file_system.write(client(), overflow, 0, &full),
            Ok(MAXIMUM_FILE_BYTE_COUNT)
        );
    }

    #[test]
    fn handles_run_out() {
        let mut file_system = FileSystem::new();
        for _ in 0..HANDLE_CAPACITY {
            assert!(
                file_system
                    .attach(client(), AccessRights::READ_ONLY)
                    .is_ok()
            );
        }
        assert_eq!(
            file_system.attach(client(), AccessRights::READ_ONLY),
            Err(FileSystemError::HandlesExhausted)
        );
    }

    #[test]
    fn nodes_run_out() {
        let (mut file_system, root) = attached();
        for index in 0..NODE_CAPACITY - 1 {
            let name = std::format!("n{index}");
            let handle = file_system
                .create(client(), root, name.as_bytes(), NodeKind::File)
                .expect("room for another node");
            file_system.clunk(client(), handle).expect("clunk");
        }
        assert_eq!(
            file_system.create(client(), root, b"one-too-many", NodeKind::File),
            Err(FileSystemError::CapacityExhausted)
        );
    }

    #[test]
    fn releasing_an_owner_drops_only_its_handles() {
        let (mut file_system, root) = attached();
        let other_root = file_system
            .attach(other_client(), AccessRights::READ_ONLY)
            .expect("attach");

        assert_eq!(file_system.release_owner(client()), 1);
        assert_eq!(
            file_system.statistics(client(), root),
            Err(FileSystemError::InvalidHandle)
        );
        assert!(file_system.statistics(other_client(), other_root).is_ok());
    }

    #[test]
    fn statistics_reports_kind_and_size() {
        let (mut file_system, root) = attached();
        let file = create_file(&mut file_system, root, b"sized");
        file_system
            .open(client(), file, OpenMode::Write)
            .expect("open");
        file_system
            .write(client(), file, 0, b"12345")
            .expect("write");

        assert_eq!(
            file_system.statistics(client(), file),
            Ok(NodeStatus {
                kind: NodeKind::File,
                byte_count: 5
            })
        );
        assert_eq!(
            file_system.statistics(client(), root),
            Ok(NodeStatus {
                kind: NodeKind::Directory,
                byte_count: 0
            })
        );
    }
}
