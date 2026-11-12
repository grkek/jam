//! Bounded command completion and live coloring for the terminal input line.
//!
//! Redraw uses a single-row, 64-byte viewport so a long command does not leave
//! stale wrapped rows on an ordinary 80-column terminal. The editor retains the
//! complete input. Only the command word is colored; arguments remain neutral.
//! ANSI-capable terminals are required, as for the existing prompt and output.

use super::{GREEN, PROMPT, RED, RESET, Terminal, Text};
use core::fmt::Write;
use terminal::{LineEditor, command_names};

/// Printable input bytes visible beside the two-column prompt.
const INPUT_VIEWPORT_BYTE_COUNT: usize = 64;

impl Terminal {
    /// Repaints the visible suffix with the cursor left at the end of the input.
    pub(super) fn redraw_line(&mut self, line: &[u8]) {
        let range = command_names::command_range(line);
        let name = line.get(range.clone()).unwrap_or(&[]);
        let visible_start = line.len().saturating_sub(INPUT_VIEWPORT_BYTE_COUNT);
        let command_start = range.start.max(visible_start);
        let command_end = range.end.max(visible_start);
        let mut output = Text::<256>::new();
        let mut append = |bytes: &[u8]| {
            let text = core::str::from_utf8(bytes).unwrap_or_else(|_| super::fail());
            output.write_str(text).unwrap_or_else(|_| super::fail());
        };
        append(b"\r\x1b[2K");
        append(PROMPT);
        append(line.get(visible_start..command_start).unwrap_or(&[]));
        if !name.is_empty() && command_start < command_end {
            append(if command_names::is_known_command(name) {
                GREEN.as_bytes()
            } else {
                RED.as_bytes()
            });
            append(line.get(command_start..command_end).unwrap_or(&[]));
            append(RESET.as_bytes());
        }
        append(line.get(command_end..).unwrap_or(&[]));
        self.write(output.as_str().as_bytes());
    }

    /// Completes a command name, showing candidates only when no extension is possible.
    pub(super) fn complete_line(&mut self, editor: &mut LineEditor) {
        if editor.complete_command() {
            self.redraw_line(editor.line());
            return;
        }
        let range = command_names::command_range(editor.line());
        if range.end != editor.line().len() {
            return;
        }
        let prefix = editor.line().get(range).unwrap_or(&[]);
        let mut displayed = false;
        for name in command_names::COMMAND_NAMES {
            if name.as_bytes().starts_with(prefix) {
                if !displayed {
                    self.write(b"\n");
                }
                self.write(name.as_bytes());
                self.write(b"\n");
                displayed = true;
            }
        }
        if displayed {
            self.redraw_line(editor.line());
        }
    }
}
