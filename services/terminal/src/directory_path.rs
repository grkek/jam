//! Bounded canonical absolute paths owned by an EL0 terminal session.
//!
//! The filesystem never receives a complete path. Terminal commands split the
//! canonical path into validated components and walk it from an attached handle.

use foundation::file_system::{MAXIMUM_WALK_COMPONENT_COUNT, is_valid_name};

/// Longest canonical absolute path tracked by the terminal, in bytes.
pub(super) const MAXIMUM_PATH_BYTE_COUNT: usize = 96;
/// Most path components below the root.
pub(super) const MAXIMUM_DEPTH: usize = MAXIMUM_WALK_COMPONENT_COUNT;

/// Canonical absolute path containing no empty, current, or parent components.
#[derive(Clone, Copy)]
pub(super) struct DirectoryPath {
    /// Fixed path storage; only its initialized prefix is exposed.
    bytes: [u8; MAXIMUM_PATH_BYTE_COUNT],
    /// Initialized byte count, including the first slash.
    length: usize,
    /// Number of components below the root.
    depth: usize,
}

impl DirectoryPath {
    /// Starts a terminal session at the root directory.
    pub(super) const fn root() -> Self {
        let mut bytes = [0; MAXIMUM_PATH_BYTE_COUNT];
        bytes[0] = b'/';
        Self {
            bytes,
            length: 1,
            depth: 0,
        }
    }

    /// Borrows the initialized canonical absolute path.
    pub(super) fn as_bytes(&self) -> &[u8] {
        &self.bytes[..self.length]
    }

    /// Returns whether the path identifies the root directory.
    pub(super) const fn is_root(&self) -> bool {
        self.depth == 0
    }

    /// Borrows the final component, or returns `None` for root.
    pub(super) fn name(&self) -> Option<&[u8]> {
        if self.is_root() {
            None
        } else {
            self.as_bytes().rsplit(|byte| *byte == b'/').next()
        }
    }

    /// Removes the last component, leaving root unchanged.
    pub(super) fn parent(&mut self) {
        if self.is_root() {
            return;
        }
        let separator_index = self
            .as_bytes()
            .iter()
            .rposition(|byte| *byte == b'/')
            .unwrap_or(0);
        self.length = separator_index.max(1);
        self.depth -= 1;
    }

    /// Appends one valid component without changing the path on failure.
    ///
    /// Rejected names, excessive depth, and oversized paths are all atomic.
    pub(super) fn append(&mut self, component: &[u8]) -> Option<()> {
        if !is_valid_name(component) || self.depth >= MAXIMUM_DEPTH {
            return None;
        }
        let separator_byte_count = usize::from(!self.is_root());
        let end = self
            .length
            .checked_add(separator_byte_count)?
            .checked_add(component.len())?;
        if end > MAXIMUM_PATH_BYTE_COUNT {
            return None;
        }

        let mut insertion_index = self.length;
        if separator_byte_count != 0 {
            self.bytes[insertion_index] = b'/';
            insertion_index += 1;
        }
        self.bytes[insertion_index..end].copy_from_slice(component);
        self.length = end;
        self.depth += 1;
        Some(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn root_has_no_parent_or_name() {
        let mut path = DirectoryPath::root();
        assert_eq!(path.as_bytes(), b"/");
        assert_eq!(path.name(), None);
        path.parent();
        assert_eq!(path.as_bytes(), b"/");
    }

    #[test]
    fn append_and_parent_preserve_canonical_form() {
        let mut path = DirectoryPath::root();
        assert_eq!(path.append(b"projects"), Some(()));
        assert_eq!(path.append(b"jam"), Some(()));
        assert_eq!(path.as_bytes(), b"/projects/jam");
        assert_eq!(path.name(), Some(&b"jam"[..]));
        path.parent();
        assert_eq!(path.as_bytes(), b"/projects");
        path.parent();
        assert_eq!(path.as_bytes(), b"/");
    }

    #[test]
    fn failed_append_does_not_change_the_path() {
        let mut path = DirectoryPath::root();
        assert_eq!(path.append(b"valid"), Some(()));
        assert_eq!(path.append(b".."), None);
        assert_eq!(path.append(b"invalid/name"), None);
        assert_eq!(path.append(&[b'a'; MAXIMUM_PATH_BYTE_COUNT]), None);
        assert_eq!(path.as_bytes(), b"/valid");
    }

    #[test]
    fn depth_is_bounded() {
        let mut path = DirectoryPath::root();
        for _ in 0..MAXIMUM_DEPTH {
            assert_eq!(path.append(b"x"), Some(()));
        }
        assert_eq!(path.append(b"x"), None);
        assert_eq!(path.depth, MAXIMUM_DEPTH);
    }
}
