//! Bounded terminal line editing and exact, descriptive command names.
//!
//! This library is independent of UART access and system calls, so input parsing
//! can be tested on the host and reused with another console transport.

#![no_std]

pub mod command_names;

/// Maximum bytes accepted in a command line.
pub const MAXIMUM_COMMAND_BYTE_COUNT: usize = 128;

/// Commands implemented by the first interactive userland slice.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Command {
    /// Display the supported commands.
    Help,
    /// Read live actor census records.
    Actors,
    /// Read diagnostic node resource counters.
    Statistics,
    /// Empty input has no effect.
    Empty,
    /// Unknown names and unexpected arguments are rejected.
    Invalid,
}

/// Parses one exact command without accepting abbreviations or extra arguments.
#[must_use]
pub fn parse_command(line: &[u8]) -> Command {
    let Ok(text) = core::str::from_utf8(line) else {
        return Command::Invalid;
    };
    let mut words = text.split_ascii_whitespace();
    let Some(name) = words.next() else {
        return Command::Empty;
    };
    if words.next().is_some() {
        return Command::Invalid;
    }
    match name {
        "help" => Command::Help,
        "actors" => Command::Actors,
        "statistics" => Command::Statistics,
        _ => Command::Invalid,
    }
}

/// What a terminal should display after processing an input byte.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InputEvent {
    /// Ignore an unsupported control byte or a swallowed line-feed.
    Ignored,
    /// Echo one accepted printable byte.
    Echo(u8),
    /// Complete the first word or display matching command names.
    Complete,
    /// Erase one previously echoed character.
    Erase,
    /// Execute the current line, then reset it.
    Submit,
    /// The entire overlong line was discarded; do not execute a prefix.
    TooLong,
    /// Cancel the current input and display a fresh prompt.
    Cancel,
}

/// Fixed-size ASCII line editor with CRLF handling and escape-sequence filtering.
pub struct LineEditor {
    bytes: [u8; MAXIMUM_COMMAND_BYTE_COUNT],
    length: usize,
    discarded: bool,
    carriage_return: bool,
    escape: u8,
}

impl Default for LineEditor {
    fn default() -> Self {
        Self::new()
    }
}

impl LineEditor {
    /// Creates an empty editor without allocating.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            bytes: [0; MAXIMUM_COMMAND_BYTE_COUNT],
            length: 0,
            discarded: false,
            carriage_return: false,
            escape: 0,
        }
    }

    /// Returns the current command bytes.
    #[must_use]
    pub fn line(&self) -> &[u8] {
        self.bytes.get(..self.length).unwrap_or(&[])
    }

    /// Clears a submitted line while preserving CRLF suppression.
    pub fn reset(&mut self) {
        self.length = 0;
        self.discarded = false;
        self.escape = 0;
    }

    /// Extends the first word to its common completion, or adds a space to an exact name.
    ///
    /// Returns true when the line changes. Arguments, discarded input, and
    /// insufficient buffer space leave the line untouched. The caller may display
    /// candidates when an ambiguous prefix cannot be extended further.
    pub fn complete_command(&mut self) -> bool {
        if self.discarded {
            return false;
        }
        let range = command_names::command_range(self.line());
        if range.end != self.length {
            return false;
        }
        let prefix = self.line().get(range.clone()).unwrap_or(&[]);
        let mut matches = command_names::COMMAND_NAMES
            .iter()
            .filter(|name| name.as_bytes().starts_with(prefix));
        let Some(first) = matches.next() else {
            return false;
        };
        let mut common_length = first.len();
        let mut unique = true;
        for candidate in matches {
            unique = false;
            common_length = first
                .bytes()
                .zip(candidate.bytes())
                .take(common_length)
                .take_while(|(left, right)| left == right)
                .count();
        }
        let exact = command_names::is_known_command(prefix);
        let completed_length = if exact { prefix.len() } else { common_length };
        let add_space = exact || unique;
        let next_length = range.start + completed_length + usize::from(add_space);
        if next_length <= self.length || next_length > self.bytes.len() {
            return false;
        }
        if !exact {
            let Some(source) = first.as_bytes().get(..completed_length) else {
                return false;
            };
            let Some(destination) = self
                .bytes
                .get_mut(range.start..range.start + completed_length)
            else {
                return false;
            };
            destination.copy_from_slice(source);
        }
        if add_space {
            let Some(destination) = self.bytes.get_mut(next_length - 1) else {
                return false;
            };
            *destination = b' ';
        }
        self.length = next_length;
        true
    }

    /// Handles one byte; overlong commands remain discarded until their newline.
    pub fn feed(&mut self, byte: u8) -> InputEvent {
        if byte == b'\n' && self.carriage_return {
            self.carriage_return = false;
            return InputEvent::Ignored;
        }
        self.carriage_return = byte == b'\r';
        if byte == 3 || byte == 21 {
            self.reset();
            return InputEvent::Cancel;
        }
        if byte == b'\r' || byte == b'\n' {
            self.escape = 0;
            return if self.discarded {
                InputEvent::TooLong
            } else {
                InputEvent::Submit
            };
        }
        if self.discarded {
            return InputEvent::Ignored;
        }
        if self.escape != 0 {
            if self.escape == 1 && matches!(byte, b'[' | b'O') {
                self.escape = 2;
            } else if self.escape == 1 || (0x40..=0x7e).contains(&byte) {
                self.escape = 0;
            }
            return InputEvent::Ignored;
        }
        if byte == 27 {
            self.escape = 1;
            return InputEvent::Ignored;
        }
        if byte == 8 || byte == 127 {
            if self.length == 0 {
                return InputEvent::Ignored;
            }
            self.length -= 1;
            return InputEvent::Erase;
        }
        if byte == b'\t' {
            return InputEvent::Complete;
        }
        if !(0x20..=0x7e).contains(&byte) {
            return InputEvent::Ignored;
        }
        let Some(destination) = self.bytes.get_mut(self.length) else {
            self.discarded = true;
            return InputEvent::Ignored;
        };
        *destination = byte;
        self.length += 1;
        InputEvent::Echo(byte)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_full_and_arguments_are_checked() {
        assert_eq!(parse_command(b" statistics \t"), Command::Statistics);
        assert_eq!(parse_command(b"stats"), Command::Invalid);
        assert_eq!(parse_command(b"actors extra"), Command::Invalid);
        assert_eq!(parse_command(b""), Command::Empty);
    }

    #[test]
    fn editing_and_crlf_do_not_duplicate_commands() {
        let mut editor = LineEditor::new();
        for byte in b"helx\x7fp" {
            editor.feed(*byte);
        }
        assert_eq!(parse_command(editor.line()), Command::Help);
        assert_eq!(editor.feed(b'\r'), InputEvent::Submit);
        editor.reset();
        assert_eq!(editor.feed(b'\n'), InputEvent::Ignored);
        assert!(editor.line().is_empty());
    }

    #[test]
    fn overlong_input_never_executes_a_truncated_command() {
        let mut editor = LineEditor::new();
        for _ in 0..=MAXIMUM_COMMAND_BYTE_COUNT {
            editor.feed(b'a');
        }
        editor.feed(127);
        assert_eq!(editor.feed(b'\n'), InputEvent::TooLong);
        editor.reset();
        assert_eq!(editor.feed(b'h'), InputEvent::Echo(b'h'));
    }

    #[test]
    fn escape_sequences_and_cancel_are_bounded() {
        let mut editor = LineEditor::new();
        for byte in b"he\x1b[1;5Dlp" {
            editor.feed(*byte);
        }
        assert_eq!(parse_command(editor.line()), Command::Help);
        assert_eq!(editor.feed(3), InputEvent::Cancel);
        assert!(editor.line().is_empty());
        assert_eq!(editor.feed(8), InputEvent::Ignored);
    }
}
