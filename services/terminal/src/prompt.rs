//! Prompt rendering: `] ` on the left, the working directory flush right as `[~/path]`.

use core::fmt::Write;

use super::{PROMPT, RESET, Terminal, Text};

const BRACE: &str = "\x1b[0;1;38;5;238m";
const TEXT: &str = "\x1b[0;38;5;250m";

/// Longest directory label in characters; longer paths keep their tail.
const MAXIMUM_LABEL_CHARACTERS: usize = 24;

/// `/` becomes `~`, `/docs/a` becomes `~/docs/a`, and long paths become `~/..tail`.
fn label(path: &str) -> Text<128> {
    let relative = path.trim_matches('/');
    let mut text = Text::new();
    if relative.is_empty() {
        text.write_str("~").ok();
        return text;
    }

    // Two characters go to the "~/" prefix.
    let budget = MAXIMUM_LABEL_CHARACTERS - 2;
    let count = relative.chars().count();
    if count > budget {
        let keep = budget - 2; // two more for the ".." marker
        let start = relative
            .char_indices()
            .nth(count - keep)
            .map_or(0, |(index, _)| index);
        write!(text, "~/..{}", relative.get(start..).unwrap_or("")).ok();
    } else {
        write!(text, "~/{relative}").ok();
    }
    text
}

impl Terminal {
    /// Starts a flicker-free update: the terminal holds its drawing until `end_update`.
    #[inline(never)]
    pub(super) fn begin_update(&mut self) {
        // ?2026 is synchronized output; terminals that don't know it ignore it.
        self.write(b"\x1b[?2026h\x1b[?25l");
    }

    /// Shows everything drawn since `begin_update` at once.
    #[inline(never)]
    pub(super) fn end_update(&mut self) {
        self.write(b"\x1b[?25h\x1b[?2026l");
    }

    /// Draws the left prompt, then the working directory at the right edge.
    #[inline(never)]
    pub(super) fn show_prompt(&mut self) {
        self.write(PROMPT);
        self.right_prompt();
    }

    /// Draws `[label]` flush right and puts the cursor back where it was.
    ///
    /// Safe to call repeatedly: it erases from the label's first column to the end
    /// of the line before drawing, so shifted or half-erased remnants disappear.
    #[inline(never)]
    pub(super) fn right_prompt(&mut self) {
        let path = core::str::from_utf8(self.working_directory.as_bytes()).unwrap_or("/");
        let label = label(path);
        let width = label.as_str().chars().count() + 2;
        self.print(format_args!(
            "\x1b7\x1b[999G\x1b[{width}D\x1b[K{BRACE}[{TEXT}{}{BRACE}]{RESET}\x1b8",
            label.as_str()
        ));
    }
}
