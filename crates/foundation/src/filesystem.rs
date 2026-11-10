//! Bounded directory messages for the first in-memory filesystem service.
//!
//! A create request carries the bare absolute ASCII path. A list request starts
//! with a little-endian u16 cursor, followed by the path. Each reply contains a
//! status byte, the next little-endian cursor, and at most one child name.
//! A listing ends with `next_cursor == 0` and an empty name; a returned child
//! always carries a cursor of at least one. Correlation and sender identity come
//! from message metadata, never from the payload.
//!
//! Version one only creates and lists directories. There is no disk persistence,
//! deletion, file content, executable loading, or public service discovery yet.

use crate::tag::Tag;

pub(crate) const PROTOCOL: u16 = 0x4653;

/// Requests one child after the supplied cursor.
pub const LIST_DIRECTORY_TAG: Tag = Tag::new(PROTOCOL, 1);
/// Creates one directory whose parent already exists.
pub const CREATE_DIRECTORY_TAG: Tag = Tag::new(PROTOCOL, 2);
/// Replies to either directory operation using the request's correlation.
pub const RESPONSE_TAG: Tag = Tag::new(PROTOCOL, 3);

/// Maximum bytes in an absolute path, including separators.
pub const MAXIMUM_PATH_BYTE_COUNT: usize = 96;
/// Bytes occupied by the listing cursor.
pub const CURSOR_BYTE_COUNT: usize = 2;
/// Status byte followed by the next little-endian cursor.
pub const RESPONSE_HEADER_BYTE_COUNT: usize = 1 + CURSOR_BYTE_COUNT;
/// Largest accepted directory request.
pub const MAXIMUM_REQUEST_BYTE_COUNT: usize = CURSOR_BYTE_COUNT + MAXIMUM_PATH_BYTE_COUNT;
/// Largest directory reply, including one child name.
pub const MAXIMUM_RESPONSE_BYTE_COUNT: usize = RESPONSE_HEADER_BYTE_COUNT + MAXIMUM_PATH_BYTE_COUNT;

/// Operation result encoded in the first reply byte.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Status {
    /// Creation succeeded, or a listing returned a child or its end marker.
    Accepted = 0,
    /// The path or request encoding is unsupported or malformed.
    InvalidRequest = 1,
    /// The directory or its parent does not exist.
    NotFound = 2,
    /// The requested directory already exists.
    AlreadyExists = 3,
    /// The bounded directory table is full.
    CapacityExhausted = 4,
}

impl Status {
    /// Decodes the status byte; `None` for a value this version does not define.
    #[must_use]
    pub const fn from_byte(byte: u8) -> Option<Self> {
        match byte {
            0 => Some(Self::Accepted),
            1 => Some(Self::InvalidRequest),
            2 => Some(Self::NotFound),
            3 => Some(Self::AlreadyExists),
            4 => Some(Self::CapacityExhausted),
            _ => None,
        }
    }
}

/// A decoded directory reply, borrowing its name from the received payload.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Response<'a> {
    /// Result of the operation.
    pub status: Status,
    /// Cursor to send in the next list request. Zero means the listing has ended.
    pub next_cursor: u16,
    /// Child name for a list reply. Empty for creation replies and at the end of a listing.
    pub name: &'a [u8],
}

/// Builds a create request (the bare path) into `buffer`; returns its length.
#[must_use]
pub fn encode_create_request(path: &[u8], buffer: &mut [u8]) -> Option<usize> {
    if path.len() > MAXIMUM_PATH_BYTE_COUNT {
        return None;
    }
    buffer.get_mut(..path.len())?.copy_from_slice(path);
    Some(path.len())
}

/// A create request's path, or `None` if it is too long.
#[must_use]
pub fn decode_create_request(payload: &[u8]) -> Option<&[u8]> {
    (payload.len() <= MAXIMUM_PATH_BYTE_COUNT).then_some(payload)
}

/// Builds a list request (cursor, then path) into `buffer`; returns its length.
#[must_use]
pub fn encode_list_request(cursor: u16, path: &[u8], buffer: &mut [u8]) -> Option<usize> {
    if path.len() > MAXIMUM_PATH_BYTE_COUNT {
        return None;
    }
    let length = CURSOR_BYTE_COUNT + path.len();
    let (cursor_bytes, path_bytes) = buffer.get_mut(..length)?.split_at_mut(CURSOR_BYTE_COUNT);
    cursor_bytes.copy_from_slice(&cursor.to_le_bytes());
    path_bytes.copy_from_slice(path);
    Some(length)
}

/// Splits a list request into its cursor and path.
#[must_use]
pub fn decode_list_request(payload: &[u8]) -> Option<(u16, &[u8])> {
    let (cursor_bytes, path) = payload.split_at_checked(CURSOR_BYTE_COUNT)?;
    if path.len() > MAXIMUM_PATH_BYTE_COUNT {
        return None;
    }
    Some((u16::from_le_bytes(cursor_bytes.try_into().ok()?), path))
}

/// Builds a reply (status, next cursor, optional name) into `buffer`; returns its length.
#[must_use]
pub fn encode_response(
    status: Status,
    next_cursor: u16,
    name: &[u8],
    buffer: &mut [u8],
) -> Option<usize> {
    if name.len() > MAXIMUM_PATH_BYTE_COUNT {
        return None;
    }
    let length = RESPONSE_HEADER_BYTE_COUNT + name.len();
    let target = buffer.get_mut(..length)?;
    let (header, name_bytes) = target.split_at_mut(RESPONSE_HEADER_BYTE_COUNT);
    let (status_byte, cursor_bytes) = header.split_at_mut(1);
    status_byte.copy_from_slice(&[status as u8]);
    cursor_bytes.copy_from_slice(&next_cursor.to_le_bytes());
    name_bytes.copy_from_slice(name);
    Some(length)
}

/// Parses a reply. `None` for a short payload, an unknown status, or an oversized name.
#[must_use]
pub fn decode_response(payload: &[u8]) -> Option<Response<'_>> {
    let (header, name) = payload.split_at_checked(RESPONSE_HEADER_BYTE_COUNT)?;
    if name.len() > MAXIMUM_PATH_BYTE_COUNT {
        return None;
    }
    let (status_byte, cursor_bytes) = header.split_at(1);
    Some(Response {
        status: Status::from_byte(*status_byte.first()?)?,
        next_cursor: u16::from_le_bytes(cursor_bytes.try_into().ok()?),
        name,
    })
}
