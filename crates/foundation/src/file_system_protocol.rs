//! Messages exchanged with the file system actor.
//!
//! A request carries one of the tags below and a payload. The reply carries
//! [`RESPONSE_TAG`] and the request's correlation. All integers are little-endian, and a
//! handle travels in its packed form.
//!
//! Request payloads:
//!
//! | Tag | Payload |
//! |---|---|
//! | attach | empty |
//! | walk | handle, name count (1 byte), then each name as a length byte and its bytes |
//! | open | handle, mode (1 byte) |
//! | create | handle, kind (1 byte), name (the rest) |
//! | read | handle, offset (8 bytes), count (2 bytes) |
//! | write | handle, offset (8 bytes), data (the rest) |
//! | read directory | handle, cursor (2 bytes) |
//! | statistics, remove, clunk | handle |
//!
//! Reply payload: a status byte, then a body that depends on the request:
//!
//! | Request | Body |
//! |---|---|
//! | attach, walk, create | the new handle |
//! | open, remove, clunk | empty |
//! | read | the bytes read |
//! | write | bytes written (2 bytes) |
//! | statistics | kind (1 byte), size (4 bytes) |
//! | read directory | next cursor (2 bytes), kind (1 byte), name (the rest) |
//!
//! A read directory reply with a next cursor of zero and no name ends the listing.
//! Every request is decoded strictly: trailing or missing bytes make it invalid.

use crate::{
    file_system::{
        DirectoryEntry, MAXIMUM_NAME_BYTE_COUNT, MAXIMUM_WALK_COMPONENT_COUNT, NodeKind,
        NodeStatus, OpenMode,
    },
    file_system_error::FileSystemError,
    file_system_handle::FileSystemHandle,
    messaging::MAXIMUM_RECORD_BYTE_COUNT,
    tag::Tag,
    wire_bytes::read_array,
};

// These numbers are placeholders. Pick values that do not collide with the tags of your
// other services. `Tag::new` is assumed to be a `const fn`; match your `tag.rs`.

/// Protocol definition of the file system.
pub const PROTOCOL: u16 = 0x0002;

/// Asks for a handle to the root directory.
pub const ATTACH_TAG: Tag = Tag::new(PROTOCOL, 0x4601);
/// Follows names from a directory handle and returns a handle to where they end.
pub const WALK_TAG: Tag = Tag::new(PROTOCOL, 0x4602);
/// Opens a handle for reading, writing, or both.
pub const OPEN_TAG: Tag = Tag::new(PROTOCOL, 0x4603);
/// Creates a file or directory inside a directory.
pub const CREATE_TAG: Tag = Tag::new(PROTOCOL, 0x4604);
/// Reads bytes from an open file.
pub const READ_TAG: Tag = Tag::new(PROTOCOL, 0x4605);
/// Writes bytes to an open file.
pub const WRITE_TAG: Tag = Tag::new(PROTOCOL, 0x4606);
/// Reads one entry from an open directory.
pub const READ_DIRECTORY_TAG: Tag = Tag::new(PROTOCOL, 0x4607);
/// Reports the kind and size of a node.
pub const STATISTICS_TAG: Tag = Tag::new(PROTOCOL, 0x4608);
/// Removes a node and releases the handle.
pub const REMOVE_TAG: Tag = Tag::new(PROTOCOL, 0x4609);
/// Releases a handle.
pub const CLUNK_TAG: Tag = Tag::new(PROTOCOL, 0x460A);
/// Tag of every reply.
pub const RESPONSE_TAG: Tag = Tag::new(PROTOCOL, 0x46FF);

/// Bytes in a packed handle.
const HANDLE_WIRE_BYTE_COUNT: usize = 8;
/// Bytes in a file offset.
const OFFSET_WIRE_BYTE_COUNT: usize = 8;
/// Bytes in a listing cursor, and in a read or write count.
const SHORT_WIRE_BYTE_COUNT: usize = 2;
/// Most bytes a single read returns.
pub const MAXIMUM_READ_BYTE_COUNT: usize = 128;
/// Most bytes a single write carries.
pub const MAXIMUM_WRITE_BYTE_COUNT: usize = 128;
/// Bytes in front of every reply body.
pub const RESPONSE_HEADER_BYTE_COUNT: usize = 1;
/// Bytes in a statistics body: kind and size.
const STATISTICS_BODY_BYTE_COUNT: usize = 5;
/// Bytes in a directory body before the name: cursor and kind.
const DIRECTORY_BODY_HEADER_BYTE_COUNT: usize = 3;
/// Size of the largest reply. The read body is the largest body.
pub const MAXIMUM_RESPONSE_BYTE_COUNT: usize = RESPONSE_HEADER_BYTE_COUNT + MAXIMUM_READ_BYTE_COUNT;

const _: () = assert!(
    HANDLE_WIRE_BYTE_COUNT <= MAXIMUM_READ_BYTE_COUNT,
    "Handle must fit in a body"
);
const _: () = assert!(
    STATISTICS_BODY_BYTE_COUNT <= MAXIMUM_READ_BYTE_COUNT,
    "Stat must fit in a body"
);
const _: () = assert!(
    DIRECTORY_BODY_HEADER_BYTE_COUNT + MAXIMUM_NAME_BYTE_COUNT <= MAXIMUM_READ_BYTE_COUNT,
    "A directory entry must fit in a body"
);
const _: () = assert!(
    MAXIMUM_READ_BYTE_COUNT < u16::MAX as usize,
    "Read counts must fit in a u16"
);
const _: () = assert!(
    MAXIMUM_RESPONSE_BYTE_COUNT <= MAXIMUM_RECORD_BYTE_COUNT,
    "A reply must fit in one message record; lower MAXIMUM_READ_BYTE_COUNT"
);
const _: () = assert!(
    HANDLE_WIRE_BYTE_COUNT + OFFSET_WIRE_BYTE_COUNT + MAXIMUM_WRITE_BYTE_COUNT
        <= MAXIMUM_RECORD_BYTE_COUNT,
    "A write must fit in one message record; lower MAXIMUM_WRITE_BYTE_COUNT"
);

/// Outcome of a request, as the first byte of every reply.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Status {
    /// The request succeeded.
    Accepted = 0,
    /// The payload could not be decoded, or the tag is unknown.
    InvalidRequest = 1,
    /// The handle was never issued, was released, or belongs to another client.
    InvalidHandle = 2,
    /// The node behind the handle has been removed.
    StaleHandle = 3,
    /// A name is empty, too long, contains a separator, or is `.` or `..`.
    InvalidName = 4,
    /// A name does not exist in the directory.
    NotFound = 5,
    /// The name is already taken.
    AlreadyExists = 6,
    /// A directory was required.
    NotADirectory = 7,
    /// A file was required.
    IsADirectory = 8,
    /// The directory still has children.
    NotEmpty = 9,
    /// The handle is already open.
    AlreadyOpen = 10,
    /// The handle has not been opened.
    NotOpen = 11,
    /// The handle's rights or open mode do not allow the operation.
    PermissionDenied = 12,
    /// The root cannot be removed.
    CannotRemoveRoot = 13,
    /// A walk named too many components.
    TooManyComponents = 14,
    /// Every node is in use.
    CapacityExhausted = 15,
    /// Every handle is in use.
    HandlesExhausted = 16,
    /// Every block is in use.
    OutOfSpace = 17,
    /// The write would pass the largest file size.
    FileTooLarge = 18,
}

impl Status {
    /// The byte that represents this status on the wire.
    #[must_use]
    pub const fn number(self) -> u8 {
        self as u8
    }

    /// Reads a status from its wire byte.
    #[must_use]
    pub const fn from_number(number: u8) -> Option<Self> {
        match number {
            0 => Some(Self::Accepted),
            1 => Some(Self::InvalidRequest),
            2 => Some(Self::InvalidHandle),
            3 => Some(Self::StaleHandle),
            4 => Some(Self::InvalidName),
            5 => Some(Self::NotFound),
            6 => Some(Self::AlreadyExists),
            7 => Some(Self::NotADirectory),
            8 => Some(Self::IsADirectory),
            9 => Some(Self::NotEmpty),
            10 => Some(Self::AlreadyOpen),
            11 => Some(Self::NotOpen),
            12 => Some(Self::PermissionDenied),
            13 => Some(Self::CannotRemoveRoot),
            14 => Some(Self::TooManyComponents),
            15 => Some(Self::CapacityExhausted),
            16 => Some(Self::HandlesExhausted),
            17 => Some(Self::OutOfSpace),
            18 => Some(Self::FileTooLarge),
            _ => None,
        }
    }

    /// The status that reports `error`. An inconsistency has none, because the actor
    /// faults instead of answering.
    #[must_use]
    pub const fn from_error(error: FileSystemError) -> Option<Self> {
        match error {
            FileSystemError::InvalidHandle => Some(Self::InvalidHandle),
            FileSystemError::StaleHandle => Some(Self::StaleHandle),
            FileSystemError::InvalidName => Some(Self::InvalidName),
            FileSystemError::NotFound => Some(Self::NotFound),
            FileSystemError::AlreadyExists => Some(Self::AlreadyExists),
            FileSystemError::NotADirectory => Some(Self::NotADirectory),
            FileSystemError::IsADirectory => Some(Self::IsADirectory),
            FileSystemError::NotEmpty => Some(Self::NotEmpty),
            FileSystemError::AlreadyOpen => Some(Self::AlreadyOpen),
            FileSystemError::NotOpen => Some(Self::NotOpen),
            FileSystemError::PermissionDenied => Some(Self::PermissionDenied),
            FileSystemError::CannotRemoveRoot => Some(Self::CannotRemoveRoot),
            FileSystemError::TooManyComponents => Some(Self::TooManyComponents),
            FileSystemError::CapacityExhausted => Some(Self::CapacityExhausted),
            FileSystemError::HandlesExhausted => Some(Self::HandlesExhausted),
            FileSystemError::OutOfSpace => Some(Self::OutOfSpace),
            FileSystemError::FileTooLarge => Some(Self::FileTooLarge),
            FileSystemError::Inconsistent => None,
        }
    }
}

/// The wire byte for an open mode.
#[must_use]
pub const fn open_mode_number(mode: OpenMode) -> u8 {
    match mode {
        OpenMode::Read => 0,
        OpenMode::Write => 1,
        OpenMode::ReadWrite => 2,
    }
}

/// Reads an open mode from its wire byte.
#[must_use]
pub const fn open_mode_from_number(number: u8) -> Option<OpenMode> {
    match number {
        0 => Some(OpenMode::Read),
        1 => Some(OpenMode::Write),
        2 => Some(OpenMode::ReadWrite),
        _ => None,
    }
}

/// The wire byte for a node kind.
#[must_use]
pub const fn node_kind_number(kind: NodeKind) -> u8 {
    match kind {
        NodeKind::Directory => 0,
        NodeKind::File => 1,
    }
}

/// Reads a node kind from its wire byte.
#[must_use]
pub const fn node_kind_from_number(number: u8) -> Option<NodeKind> {
    match number {
        0 => Some(NodeKind::Directory),
        1 => Some(NodeKind::File),
        _ => None,
    }
}

/// Copies `bytes` into `buffer` at `offset` and returns the offset after them.
fn put(buffer: &mut [u8], offset: usize, bytes: &[u8]) -> Option<usize> {
    let end = offset.checked_add(bytes.len())?;
    buffer.get_mut(offset..end)?.copy_from_slice(bytes);
    Some(end)
}

fn read_handle(payload: &[u8]) -> Option<FileSystemHandle> {
    let bytes: [u8; HANDLE_WIRE_BYTE_COUNT] = read_array(payload, 0)?;
    Some(FileSystemHandle::from_packed(u64::from_le_bytes(bytes)))
}

fn read_offset(payload: &[u8], position: usize) -> Option<u64> {
    let bytes: [u8; OFFSET_WIRE_BYTE_COUNT] = read_array(payload, position)?;
    Some(u64::from_le_bytes(bytes))
}

fn read_short(payload: &[u8], position: usize) -> Option<u16> {
    let bytes: [u8; SHORT_WIRE_BYTE_COUNT] = read_array(payload, position)?;
    Some(u16::from_le_bytes(bytes))
}

/// A request that names a handle and nothing else: statistics, remove, and clunk.
///
/// Returns `None` unless the payload is exactly a handle.
#[must_use]
pub fn decode_handle_request(payload: &[u8]) -> Option<FileSystemHandle> {
    if payload.len() != HANDLE_WIRE_BYTE_COUNT {
        return None;
    }
    read_handle(payload)
}

/// Serializes a request that names a handle and nothing else.
#[must_use]
pub fn encode_handle_request(handle: FileSystemHandle, buffer: &mut [u8]) -> Option<usize> {
    put(buffer, 0, &handle.to_packed().to_le_bytes())
}

/// A decoded walk request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WalkRequest<'payload> {
    /// The directory to start from.
    pub handle: FileSystemHandle,
    names: [&'payload [u8]; MAXIMUM_WALK_COMPONENT_COUNT],
    name_count: usize,
}

impl<'payload> WalkRequest<'payload> {
    /// The components to follow, in order.
    #[must_use]
    pub fn names(&self) -> &[&'payload [u8]] {
        self.names.get(..self.name_count).unwrap_or(&[])
    }
}

/// Reads a walk request. Returns `None` when it is malformed or names too many components.
#[must_use]
pub fn decode_walk_request(payload: &[u8]) -> Option<WalkRequest<'_>> {
    let handle = read_handle(payload)?;
    let name_count = usize::from(*payload.get(HANDLE_WIRE_BYTE_COUNT)?);
    if name_count > MAXIMUM_WALK_COMPONENT_COUNT {
        return None;
    }

    let mut names: [&[u8]; MAXIMUM_WALK_COMPONENT_COUNT] = [&[]; MAXIMUM_WALK_COMPONENT_COUNT];
    let mut position = HANDLE_WIRE_BYTE_COUNT + 1;
    for slot in names.iter_mut().take(name_count) {
        let length = usize::from(*payload.get(position)?);
        *slot = payload.get(position + 1..position + 1 + length)?;
        position += 1 + length;
    }
    if position != payload.len() {
        return None;
    }
    Some(WalkRequest {
        handle,
        names,
        name_count,
    })
}

/// Serializes a walk request. Returns `None` when `names` has too many components or the
/// buffer is too small.
#[must_use]
pub fn encode_walk_request(
    handle: FileSystemHandle,
    names: &[&[u8]],
    buffer: &mut [u8],
) -> Option<usize> {
    if names.len() > MAXIMUM_WALK_COMPONENT_COUNT {
        return None;
    }
    let mut position = put(buffer, 0, &handle.to_packed().to_le_bytes())?;
    position = put(buffer, position, &[u8::try_from(names.len()).ok()?])?;
    for name in names {
        position = put(buffer, position, &[u8::try_from(name.len()).ok()?])?;
        position = put(buffer, position, name)?;
    }
    Some(position)
}

/// A decoded open request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OpenRequest {
    /// The handle to open.
    pub handle: FileSystemHandle,
    /// How it will be used.
    pub mode: OpenMode,
}

/// Reads an open request. Returns `None` when it is malformed.
#[must_use]
pub fn decode_open_request(payload: &[u8]) -> Option<OpenRequest> {
    if payload.len() != HANDLE_WIRE_BYTE_COUNT + 1 {
        return None;
    }
    let mode = open_mode_from_number(*payload.get(HANDLE_WIRE_BYTE_COUNT)?)?;
    Some(OpenRequest {
        handle: read_handle(payload)?,
        mode,
    })
}

/// Serializes an open request.
#[must_use]
pub fn encode_open_request(
    handle: FileSystemHandle,
    mode: OpenMode,
    buffer: &mut [u8],
) -> Option<usize> {
    let position = put(buffer, 0, &handle.to_packed().to_le_bytes())?;
    put(buffer, position, &[open_mode_number(mode)])
}

/// A decoded create request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CreateRequest<'payload> {
    /// The directory to create in.
    pub handle: FileSystemHandle,
    /// Directory or file.
    pub kind: NodeKind,
    /// The new node's name. The file system validates it.
    pub name: &'payload [u8],
}

/// Reads a create request. Returns `None` when it is malformed.
#[must_use]
pub fn decode_create_request(payload: &[u8]) -> Option<CreateRequest<'_>> {
    let handle = read_handle(payload)?;
    let kind = node_kind_from_number(*payload.get(HANDLE_WIRE_BYTE_COUNT)?)?;
    let name = payload.get(HANDLE_WIRE_BYTE_COUNT + 1..)?;
    Some(CreateRequest { handle, kind, name })
}

/// Serializes a create request.
#[must_use]
pub fn encode_create_request(
    handle: FileSystemHandle,
    kind: NodeKind,
    name: &[u8],
    buffer: &mut [u8],
) -> Option<usize> {
    let mut position = put(buffer, 0, &handle.to_packed().to_le_bytes())?;
    position = put(buffer, position, &[node_kind_number(kind)])?;
    put(buffer, position, name)
}

/// A decoded read request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReadRequest {
    /// The open file.
    pub handle: FileSystemHandle,
    /// Where in the file to start.
    pub offset: u64,
    /// Most bytes to return, at most [`MAXIMUM_READ_BYTE_COUNT`].
    pub count: u16,
}

/// Reads a read request. Returns `None` when it is malformed or asks for too many bytes.
#[must_use]
pub fn decode_read_request(payload: &[u8]) -> Option<ReadRequest> {
    if payload.len() != HANDLE_WIRE_BYTE_COUNT + OFFSET_WIRE_BYTE_COUNT + SHORT_WIRE_BYTE_COUNT {
        return None;
    }
    let count = read_short(payload, HANDLE_WIRE_BYTE_COUNT + OFFSET_WIRE_BYTE_COUNT)?;
    if usize::from(count) > MAXIMUM_READ_BYTE_COUNT {
        return None;
    }
    Some(ReadRequest {
        handle: read_handle(payload)?,
        offset: read_offset(payload, HANDLE_WIRE_BYTE_COUNT)?,
        count,
    })
}

/// Serializes a read request.
#[must_use]
pub fn encode_read_request(
    handle: FileSystemHandle,
    offset: u64,
    count: u16,
    buffer: &mut [u8],
) -> Option<usize> {
    let mut position = put(buffer, 0, &handle.to_packed().to_le_bytes())?;
    position = put(buffer, position, &offset.to_le_bytes())?;
    put(buffer, position, &count.to_le_bytes())
}

/// A decoded write request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WriteRequest<'payload> {
    /// The open file.
    pub handle: FileSystemHandle,
    /// Where in the file to start.
    pub offset: u64,
    /// The bytes to write, at most [`MAXIMUM_WRITE_BYTE_COUNT`].
    pub data: &'payload [u8],
}

/// Reads a write request. Returns `None` when it is malformed or carries too many bytes.
#[must_use]
pub fn decode_write_request(payload: &[u8]) -> Option<WriteRequest<'_>> {
    let data = payload.get(HANDLE_WIRE_BYTE_COUNT + OFFSET_WIRE_BYTE_COUNT..)?;
    if data.len() > MAXIMUM_WRITE_BYTE_COUNT {
        return None;
    }
    Some(WriteRequest {
        handle: read_handle(payload)?,
        offset: read_offset(payload, HANDLE_WIRE_BYTE_COUNT)?,
        data,
    })
}

/// Serializes a write request.
#[must_use]
pub fn encode_write_request(
    handle: FileSystemHandle,
    offset: u64,
    data: &[u8],
    buffer: &mut [u8],
) -> Option<usize> {
    let mut position = put(buffer, 0, &handle.to_packed().to_le_bytes())?;
    position = put(buffer, position, &offset.to_le_bytes())?;
    put(buffer, position, data)
}

/// A decoded read directory request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReadDirectoryRequest {
    /// The open directory.
    pub handle: FileSystemHandle,
    /// Zero to start, or the cursor of the previous entry.
    pub cursor: u16,
}

/// Reads a read directory request. Returns `None` when it is malformed.
#[must_use]
pub fn decode_read_directory_request(payload: &[u8]) -> Option<ReadDirectoryRequest> {
    if payload.len() != HANDLE_WIRE_BYTE_COUNT + SHORT_WIRE_BYTE_COUNT {
        return None;
    }
    Some(ReadDirectoryRequest {
        handle: read_handle(payload)?,
        cursor: read_short(payload, HANDLE_WIRE_BYTE_COUNT)?,
    })
}

/// Serializes a read directory request.
#[must_use]
pub fn encode_read_directory_request(
    handle: FileSystemHandle,
    cursor: u16,
    buffer: &mut [u8],
) -> Option<usize> {
    let position = put(buffer, 0, &handle.to_packed().to_le_bytes())?;
    put(buffer, position, &cursor.to_le_bytes())
}

/// Builds a reply from a status and a body. Returns the reply length.
#[must_use]
pub fn encode_response(status: Status, body: &[u8], reply: &mut [u8]) -> Option<usize> {
    let position = put(reply, 0, &[status.number()])?;
    put(reply, position, body)
}

/// Builds the reply that reports `error`. Returns `None` for an inconsistency, which the
/// actor answers by faulting.
#[must_use]
pub fn encode_error(error: FileSystemError, reply: &mut [u8]) -> Option<usize> {
    encode_response(Status::from_error(error)?, &[], reply)
}

/// Builds the reply to a request that returns nothing.
#[must_use]
pub fn encode_unit_result(result: Result<(), FileSystemError>, reply: &mut [u8]) -> Option<usize> {
    match result {
        Ok(()) => encode_response(Status::Accepted, &[], reply),
        Err(error) => encode_error(error, reply),
    }
}

/// Builds the reply to a request that returns a handle: attach, walk, and create.
#[must_use]
pub fn encode_handle_result(
    result: Result<FileSystemHandle, FileSystemError>,
    reply: &mut [u8],
) -> Option<usize> {
    match result {
        Ok(handle) => encode_response(Status::Accepted, &handle.to_packed().to_le_bytes(), reply),
        Err(error) => encode_error(error, reply),
    }
}

/// Builds the reply to a write.
#[must_use]
pub fn encode_count_result(
    result: Result<usize, FileSystemError>,
    reply: &mut [u8],
) -> Option<usize> {
    match result {
        Ok(count) => {
            let count = u16::try_from(count).ok()?;
            encode_response(Status::Accepted, &count.to_le_bytes(), reply)
        }
        Err(error) => encode_error(error, reply),
    }
}

/// Builds the reply to a statistics.
#[must_use]
pub fn encode_statistics_result(
    result: Result<NodeStatus, FileSystemError>,
    reply: &mut [u8],
) -> Option<usize> {
    match result {
        Ok(status) => {
            let [size_0, size_1, size_2, size_3] = status.byte_count.to_le_bytes();
            encode_response(
                Status::Accepted,
                &[
                    node_kind_number(status.kind),
                    size_0,
                    size_1,
                    size_2,
                    size_3,
                ],
                reply,
            )
        }
        Err(error) => encode_error(error, reply),
    }
}

/// Builds the reply to a read directory. `Ok(None)` ends the listing.
#[must_use]
pub fn encode_directory_result(
    result: Result<Option<DirectoryEntry<'_>>, FileSystemError>,
    reply: &mut [u8],
) -> Option<usize> {
    match result {
        Ok(Some(entry)) => {
            let [cursor_0, cursor_1] = entry.next_cursor.to_le_bytes();
            let position = put(
                reply,
                0,
                &[
                    Status::Accepted.number(),
                    cursor_0,
                    cursor_1,
                    node_kind_number(entry.kind),
                ],
            )?;
            put(reply, position, entry.name)
        }
        Ok(None) => encode_response(Status::Accepted, &[0, 0, 0], reply),
        Err(error) => encode_error(error, reply),
    }
}

/// The part of `reply` where a read can place its bytes directly, so they are not copied
/// twice. Pass it to the file system, then finish with [`encode_read_result`].
#[must_use]
pub fn read_response_body(reply: &mut [u8], count: u16) -> Option<&mut [u8]> {
    reply.get_mut(RESPONSE_HEADER_BYTE_COUNT..=usize::from(count))
}

/// Completes the reply to a read whose bytes are already in [`read_response_body`].
#[must_use]
pub fn encode_read_result(
    result: Result<usize, FileSystemError>,
    reply: &mut [u8],
) -> Option<usize> {
    match result {
        Ok(count) => {
            let length = RESPONSE_HEADER_BYTE_COUNT + count;
            if length > reply.len() {
                return None;
            }
            *reply.first_mut()? = Status::Accepted.number();
            Some(length)
        }
        Err(error) => encode_error(error, reply),
    }
}

/// Splits a reply into its status and body.
#[must_use]
pub fn decode_response(payload: &[u8]) -> Option<(Status, &[u8])> {
    let status = Status::from_number(*payload.first()?)?;
    Some((status, payload.get(RESPONSE_HEADER_BYTE_COUNT..)?))
}

/// Reads the handle in the body of an attach, walk, or create reply.
#[must_use]
pub fn decode_handle_body(body: &[u8]) -> Option<FileSystemHandle> {
    decode_handle_request(body)
}

/// Reads the byte count in the body of a write reply.
#[must_use]
pub fn decode_count_body(body: &[u8]) -> Option<u16> {
    if body.len() != SHORT_WIRE_BYTE_COUNT {
        return None;
    }
    read_short(body, 0)
}

/// Reads the body of a statistics reply.
#[must_use]
pub fn decode_statistics_body(body: &[u8]) -> Option<NodeStatus> {
    if body.len() != STATISTICS_BODY_BYTE_COUNT {
        return None;
    }
    let kind = node_kind_from_number(*body.first()?)?;
    let size_bytes: [u8; 4] = read_array(body, 1)?;
    Some(NodeStatus {
        kind,
        byte_count: u32::from_le_bytes(size_bytes),
    })
}

/// A decoded read directory reply.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DirectoryBody<'body> {
    /// Cursor that continues the listing. Zero ends it.
    pub next_cursor: u16,
    /// Directory or file.
    pub kind: NodeKind,
    /// The entry's name. Empty when the listing has ended.
    pub name: &'body [u8],
}

impl DirectoryBody<'_> {
    /// Whether the listing has ended.
    #[must_use]
    pub const fn is_end(&self) -> bool {
        self.next_cursor == 0
    }
}

/// Reads the body of a read directory reply.
#[must_use]
pub fn decode_directory_body(body: &[u8]) -> Option<DirectoryBody<'_>> {
    Some(DirectoryBody {
        next_cursor: read_short(body, 0)?,
        kind: node_kind_from_number(*body.get(SHORT_WIRE_BYTE_COUNT)?)?,
        name: body.get(DIRECTORY_BODY_HEADER_BYTE_COUNT..)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_handle() -> FileSystemHandle {
        FileSystemHandle::new(5, 12)
    }

    #[test]
    fn status_numbers_round_trip() {
        for number in 0..=18 {
            let status = Status::from_number(number).expect("a known status");
            assert_eq!(status.number(), number);
        }
        assert_eq!(Status::from_number(19), None);
    }

    #[test]
    fn an_inconsistency_has_no_status() {
        assert_eq!(Status::from_error(FileSystemError::Inconsistent), None);
        assert_eq!(
            Status::from_error(FileSystemError::NotFound),
            Some(Status::NotFound)
        );
    }

    #[test]
    fn handle_request_round_trips_and_rejects_extra_bytes() {
        let mut buffer = [0_u8; 16];
        let length = encode_handle_request(sample_handle(), &mut buffer).expect("fits");
        assert_eq!(
            decode_handle_request(buffer.get(..length).expect("in range")),
            Some(sample_handle())
        );
        assert_eq!(
            decode_handle_request(buffer.get(..=length).expect("in range")),
            None
        );
        assert_eq!(
            decode_handle_request(buffer.get(..length - 1).expect("in range")),
            None
        );
    }

    #[test]
    fn walk_request_round_trips() {
        let names: [&[u8]; 2] = [b"documents", b"readme"];
        let mut buffer = [0_u8; 64];
        let length = encode_walk_request(sample_handle(), &names, &mut buffer).expect("fits");

        let request =
            decode_walk_request(buffer.get(..length).expect("in range")).expect("decodes");
        assert_eq!(request.handle, sample_handle());
        assert_eq!(request.names(), &names);
    }

    #[test]
    fn empty_walk_request_round_trips() {
        let names: [&[u8]; 0] = [];
        let mut buffer = [0_u8; 16];
        let length = encode_walk_request(sample_handle(), &names, &mut buffer).expect("fits");
        let request =
            decode_walk_request(buffer.get(..length).expect("in range")).expect("decodes");
        assert!(request.names().is_empty());
    }

    #[test]
    fn walk_with_too_many_names_is_refused() {
        let names: [&[u8]; MAXIMUM_WALK_COMPONENT_COUNT + 1] =
            [b"a"; MAXIMUM_WALK_COMPONENT_COUNT + 1];
        let mut buffer = [0_u8; 128];
        assert_eq!(
            encode_walk_request(sample_handle(), &names, &mut buffer),
            None
        );

        // A forged count must be refused by the decoder as well.
        let mut forged = [0_u8; 16];
        let length = encode_handle_request(sample_handle(), &mut forged).expect("fits");
        *forged.get_mut(length).expect("room") = 200;
        assert_eq!(
            decode_walk_request(forged.get(..=length).expect("in range")),
            None
        );
    }

    #[test]
    fn walk_with_a_truncated_name_is_refused() {
        let names: [&[u8]; 1] = [b"documents"];
        let mut buffer = [0_u8; 32];
        let length = encode_walk_request(sample_handle(), &names, &mut buffer).expect("fits");
        assert_eq!(
            decode_walk_request(buffer.get(..length - 1).expect("in range")),
            None
        );
        assert_eq!(
            decode_walk_request(buffer.get(..length).expect("in range"))
                .map(|request| request.handle),
            Some(sample_handle())
        );
    }

    #[test]
    fn open_request_round_trips_and_rejects_unknown_modes() {
        let mut buffer = [0_u8; 16];
        let length =
            encode_open_request(sample_handle(), OpenMode::ReadWrite, &mut buffer).expect("fits");
        assert_eq!(
            decode_open_request(buffer.get(..length).expect("in range")),
            Some(OpenRequest {
                handle: sample_handle(),
                mode: OpenMode::ReadWrite
            })
        );

        *buffer.get_mut(length - 1).expect("in range") = 9;
        assert_eq!(
            decode_open_request(buffer.get(..length).expect("in range")),
            None
        );
    }

    #[test]
    fn create_request_round_trips() {
        let mut buffer = [0_u8; 64];
        let length = encode_create_request(
            sample_handle(),
            NodeKind::Directory,
            b"documents",
            &mut buffer,
        )
        .expect("fits");
        assert_eq!(
            decode_create_request(buffer.get(..length).expect("in range")),
            Some(CreateRequest {
                handle: sample_handle(),
                kind: NodeKind::Directory,
                name: b"documents"
            })
        );
    }

    #[test]
    fn read_request_round_trips_and_limits_the_count() {
        let mut buffer = [0_u8; 32];
        let length = encode_read_request(sample_handle(), 300, 64, &mut buffer).expect("fits");
        assert_eq!(
            decode_read_request(buffer.get(..length).expect("in range")),
            Some(ReadRequest {
                handle: sample_handle(),
                offset: 300,
                count: 64
            })
        );

        let too_many = u16::try_from(MAXIMUM_READ_BYTE_COUNT + 1).expect("fits in a u16");
        let length = encode_read_request(sample_handle(), 0, too_many, &mut buffer).expect("fits");
        assert_eq!(
            decode_read_request(buffer.get(..length).expect("in range")),
            None
        );
    }

    #[test]
    fn write_request_round_trips_and_limits_the_data() {
        let mut buffer = [0_u8; 256];
        let length = encode_write_request(sample_handle(), 7, b"hello", &mut buffer).expect("fits");
        assert_eq!(
            decode_write_request(buffer.get(..length).expect("in range")),
            Some(WriteRequest {
                handle: sample_handle(),
                offset: 7,
                data: b"hello"
            })
        );

        let oversized = [0_u8; MAXIMUM_WRITE_BYTE_COUNT + 1];
        let length =
            encode_write_request(sample_handle(), 0, &oversized, &mut buffer).expect("fits");
        assert_eq!(
            decode_write_request(buffer.get(..length).expect("in range")),
            None
        );
    }

    #[test]
    fn read_directory_request_round_trips() {
        let mut buffer = [0_u8; 16];
        let length = encode_read_directory_request(sample_handle(), 3, &mut buffer).expect("fits");
        assert_eq!(
            decode_read_directory_request(buffer.get(..length).expect("in range")),
            Some(ReadDirectoryRequest {
                handle: sample_handle(),
                cursor: 3
            })
        );
    }

    #[test]
    fn handle_reply_round_trips() {
        let mut reply = [0_u8; MAXIMUM_RESPONSE_BYTE_COUNT];
        let length = encode_handle_result(Ok(sample_handle()), &mut reply).expect("fits");
        let (status, body) =
            decode_response(reply.get(..length).expect("in range")).expect("decodes");
        assert_eq!(status, Status::Accepted);
        assert_eq!(decode_handle_body(body), Some(sample_handle()));
    }

    #[test]
    fn error_reply_carries_only_a_status() {
        let mut reply = [0_u8; MAXIMUM_RESPONSE_BYTE_COUNT];
        let length = encode_unit_result(Err(FileSystemError::NotFound), &mut reply).expect("fits");
        assert_eq!(
            decode_response(reply.get(..length).expect("in range")),
            Some((Status::NotFound, &[][..]))
        );
    }

    #[test]
    fn an_inconsistency_produces_no_reply() {
        let mut reply = [0_u8; MAXIMUM_RESPONSE_BYTE_COUNT];
        assert_eq!(
            encode_unit_result(Err(FileSystemError::Inconsistent), &mut reply),
            None
        );
    }

    #[test]
    fn count_reply_round_trips() {
        let mut reply = [0_u8; MAXIMUM_RESPONSE_BYTE_COUNT];
        let length = encode_count_result(Ok(42), &mut reply).expect("fits");
        let (_, body) = decode_response(reply.get(..length).expect("in range")).expect("decodes");
        assert_eq!(decode_count_body(body), Some(42));
    }

    #[test]
    fn statistics_reply_round_trips() {
        let status = NodeStatus {
            kind: NodeKind::File,
            byte_count: 70_000,
        };
        let mut reply = [0_u8; MAXIMUM_RESPONSE_BYTE_COUNT];
        let length = encode_statistics_result(Ok(status), &mut reply).expect("fits");
        let (_, body) = decode_response(reply.get(..length).expect("in range")).expect("decodes");
        assert_eq!(decode_statistics_body(body), Some(status));
    }

    #[test]
    fn directory_reply_round_trips_and_ends() {
        let entry = DirectoryEntry {
            next_cursor: 4,
            kind: NodeKind::Directory,
            name: b"documents",
        };
        let mut reply = [0_u8; MAXIMUM_RESPONSE_BYTE_COUNT];
        let length = encode_directory_result(Ok(Some(entry)), &mut reply).expect("fits");
        let (_, body) = decode_response(reply.get(..length).expect("in range")).expect("decodes");
        let listing = decode_directory_body(body).expect("decodes");
        assert_eq!(listing.next_cursor, 4);
        assert_eq!(listing.kind, NodeKind::Directory);
        assert_eq!(listing.name, b"documents");
        assert!(!listing.is_end());

        let length = encode_directory_result(Ok(None), &mut reply).expect("fits");
        let (_, body) = decode_response(reply.get(..length).expect("in range")).expect("decodes");
        assert!(decode_directory_body(body).expect("decodes").is_end());
    }

    #[test]
    fn read_reply_is_built_in_place() {
        let mut reply = [0_u8; MAXIMUM_RESPONSE_BYTE_COUNT];
        let destination = read_response_body(&mut reply, 3).expect("room for the bytes");
        destination.copy_from_slice(b"abc");

        let length = encode_read_result(Ok(3), &mut reply).expect("fits");
        assert_eq!(length, 4);
        assert_eq!(
            decode_response(reply.get(..length).expect("in range")),
            Some((Status::Accepted, &b"abc"[..]))
        );
    }

    #[test]
    fn read_reply_has_room_for_the_largest_read() {
        let mut reply = [0_u8; MAXIMUM_RESPONSE_BYTE_COUNT];
        let count = u16::try_from(MAXIMUM_READ_BYTE_COUNT).expect("fits in a u16");
        assert!(read_response_body(&mut reply, count).is_some());
    }
}
