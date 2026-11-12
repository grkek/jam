//! Why a file system request was refused.

/// Failure of a file system operation.
///
/// A refused operation changes nothing, except `remove`, which always releases the handle
/// it was given, as in 9P.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FileSystemError {
    /// The handle was never issued, was released, or belongs to another client.
    InvalidHandle,
    /// The handle is valid, but the node it referred to has been removed.
    StaleHandle,
    /// A name is empty, too long, contains a separator, or is `.` or `..`.
    InvalidName,
    /// A name does not exist in the directory.
    NotFound,
    /// The name is already taken in the directory.
    AlreadyExists,
    /// The operation needs a directory and the node is a file.
    NotADirectory,
    /// The operation needs a file and the node is a directory.
    IsADirectory,
    /// The directory still has children.
    NotEmpty,
    /// The handle has already been opened.
    AlreadyOpen,
    /// The handle has not been opened.
    NotOpen,
    /// The handle's rights or open mode do not allow the operation.
    PermissionDenied,
    /// The root of the file system cannot be removed.
    CannotRemoveRoot,
    /// A walk named more components than a single walk may traverse.
    TooManyComponents,
    /// Every node is in use.
    CapacityExhausted,
    /// Every handle is in use.
    HandlesExhausted,
    /// Every block is in use.
    OutOfSpace,
    /// The write would grow the file past the largest supported size.
    FileTooLarge,
    /// The internal tables disagree with each other. The owning actor should fault.
    Inconsistent,
}
