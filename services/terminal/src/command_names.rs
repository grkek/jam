//! Command names shared by completion and live command highlighting.
//!
//! Only implemented commands are advertised. Aliases complete and highlight just
//! like their full names. Matching is case-sensitive, as it is in the dispatcher.
//! Arguments are left untouched; path and option completion are future additions.

/// Names accepted by the interactive terminal, including aliases.
pub const COMMAND_NAMES: &[&str] = &[
    "help",
    "?",
    "actors",
    "actor",
    "tree",
    "memory",
    "statistics",
    "stack",
    "version",
    "clear",
    "list-directory",
    "ld",
    "make-directory",
    "md",
    "change-directory",
    "cd",
    "working-directory",
    "wd",
];

/// Whether a complete first word is a known command.
#[must_use]
pub fn is_known_command(name: &[u8]) -> bool {
    COMMAND_NAMES
        .iter()
        .any(|command| command.as_bytes() == name)
}

/// Byte range of the first command word, excluding whitespace and arguments.
#[must_use]
pub fn command_range(line: &[u8]) -> core::ops::Range<usize> {
    let start = line
        .iter()
        .position(|byte| !byte.is_ascii_whitespace())
        .unwrap_or(line.len());
    let end = line[start..]
        .iter()
        .position(u8::is_ascii_whitespace)
        .map_or(line.len(), |offset| start + offset);
    start..end
}
