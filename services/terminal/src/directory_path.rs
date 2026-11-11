//! Bounded canonical paths owned by a terminal session.
//!
//! Components are appended only after the caller validates traversed directories.
//! The filesystem service remains authoritative for existence and creation.

use foundation::filesystem::MAXIMUM_PATH_BYTE_COUNT;

/// Absolute path with no empty, dot, or parent components.
#[derive(Clone, Copy)]
pub(super) struct DirectoryPath {
    /// Fixed storage; only the initialized prefix is exposed.
    bytes: [u8; MAXIMUM_PATH_BYTE_COUNT],
    /// Initialized bytes, including the leading slash.
    length: usize,
}

impl DirectoryPath {
    /// Starts a terminal session at the root.
    pub(super) fn root() -> Self {
        let mut bytes = [0; MAXIMUM_PATH_BYTE_COUNT];
        if let Some(first) = bytes.first_mut() {
            *first = b'/';
        }
        Self { bytes, length: 1 }
    }

    /// Borrows the canonical absolute path.
    pub(super) fn as_bytes(&self) -> &[u8] {
        self.bytes.get(..self.length).unwrap_or(b"/")
    }

    /// Moves up one component, remaining at root when already there.
    pub(super) fn parent(&mut self) {
        self.length = self
            .as_bytes()
            .iter()
            .rposition(|byte| *byte == b'/')
            .unwrap_or(0)
            .max(1);
    }

    /// Appends a printable component, leaving this path unchanged on failure.
    pub(super) fn append(&mut self, component: &[u8]) -> Option<()> {
        if component.is_empty()
            || component == b"."
            || component == b".."
            || component.contains(&b'/')
            || !component.iter().all(u8::is_ascii_graphic)
        {
            return None;
        }
        let separator_count = usize::from(self.length > 1);
        let end = self
            .length
            .checked_add(separator_count)?
            .checked_add(component.len())?;
        let target = self.bytes.get_mut(self.length..end)?;
        if separator_count == 1 {
            let (separator, name) = target.split_at_mut(1);
            separator.copy_from_slice(b"/");
            name.copy_from_slice(component);
        } else {
            target.copy_from_slice(component);
        }
        self.length = end;
        Some(())
    }
}
