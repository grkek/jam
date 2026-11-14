//! EL0 terminal actor: command editing, observations, and directory requests.
//!
//! Every console byte travels through actor messages. This actor has no device
//! mapping and no direct access to kernel structures. The driver address arrives
//! in the boot-authenticated initial x0 register; subsequent exchanges require its authenticated sender.
//!
//! Stack discipline: this actor runs on a small fixed EL0 stack. Every command
//! handler is `#[inline(never)]` so only one handler's frame is live at a time,
//! and the census snapshot is filled in place instead of being returned by value.

#![no_std]
#![no_main]

mod directory_commands;
mod directory_path;
mod line_display;
mod prompt;

use core::cmp::Reverse;
use core::fmt::{self, Write};
use foundation::{
    address::LocalAddress, console, file_system_handle::FileSystemHandle, name::Name,
    observation::State, system_call_status::SystemCallStatus, tag::Tag,
};
use runtime::inbox::find_actor_by_name;
use runtime::{
    allocator::{self, Allocation},
    inbox::{self, MessageSelector},
    observation,
};
use terminal::{InputEvent, LineEditor};

const RESET: &str = "\x1b[0m";
const BOLD: &str = "\x1b[1m";
const DIM: &str = "\x1b[2m";
const RED: &str = "\x1b[1;91m";
const GREEN: &str = "\x1b[1;32m";
const YELLOW: &str = "\x1b[1;33m";
const CYAN: &str = "\x1b[1;36m";

/// Width of the usage bars in characters.
const BAR_WIDTH: u64 = 24;
/// Use `█`/`░` for bars. Set to false for pure-ASCII `#`/`.` on terminals without UTF-8.
const UNICODE_BARS: bool = true;
/// Bytes per user-mapped memory page.
const MEMORY_PAGE_BYTE_COUNT: usize = 4096;
/// Memory test size when no explicit size is given, in KiB.
const DEFAULT_MEMORY_TEST_KIBIBYTES: usize = 16;
/// Frames per mebibyte (4096-byte frames).
const FRAMES_PER_MEBIBYTE: u64 = 1024 * 1024 / 4096;
/// Usage percentage at which a bar turns yellow.
const WARNING_PERCENT: u64 = 60;
/// Usage percentage at which a bar turns red.
const CRITICAL_PERCENT: u64 = 85;

/// Most actors a single census snapshot keeps; bounds this actor's stack use.
const SNAPSHOT_LIMIT: usize = 32;
/// One console payload per formatting buffer; bounds stack use and avoids split writes.
const PRINT_CHUNK: usize = console::MAXIMUM_OUTPUT_BYTE_COUNT;
/// Maximum admitted console writes awaiting acknowledgements.
const MAXIMUM_OUTSTANDING_OUTPUT_COUNT: usize = 4;
/// Most words a command line may hold (the command plus its arguments).
const MAXIMUM_WORDS: usize = 8;

/// The colored prompt shown before each input line.
const PROMPT: &[u8] = b"\x1b[1;32m]\x1b[0m ";
/// Clears the screen and homes the cursor.
const CLEAR_SCREEN: &[u8] = b"\x1b[2J\x1b[H";

/// Ends this actor through the existing fault path if its transport fails.
fn fail() -> ! {
    // SAFETY: A deliberate EL0 breakpoint ends this actor, not the kernel.
    unsafe {
        core::arch::asm!("brk #1", options(noreturn));
    }
}

/// First violation detected while checking the caller's newly allocated pages.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MemoryTestFailure {
    /// A newly allocated byte was not initialised to zero.
    NotZero {
        page_index: usize,
        byte_offset: usize,
        actual: u8,
    },
    /// A previously written byte did not retain its expected value.
    Corrupted {
        page_index: usize,
        byte_offset: usize,
        expected: u8,
        actual: u8,
    },
}

/// Different contents for different pages and positions, including page boundaries.
fn memory_test_pattern(page_index: usize, byte_offset: usize) -> u8 {
    (page_index.wrapping_mul(131)
        ^ byte_offset.wrapping_mul(47)
        ^ (byte_offset >> 4)
        ^ (byte_offset >> 9)
        ^ 0xa5) as u8
}

/// Tests every byte without allocating a second buffer or consuming the EL0 stack.
///
/// # Safety invariant
/// The kernel has exclusively mapped the returned allocation as writable,
/// readable, non-executable EL0 pages, valid until `release_pages` succeeds.
#[inline(never)]
fn verify_memory_allocation(allocation: Allocation) -> Result<(), MemoryTestFailure> {
    for page_index in 0..allocation.page_count() {
        let page_address = allocation.start_address() + page_index * MEMORY_PAGE_BYTE_COUNT;
        for byte_offset in 0..MEMORY_PAGE_BYTE_COUNT {
            // SAFETY: The kernel returned these mapped pages to the calling actor;
            // each byte is within the live allocation and reading it is permitted.
            let actual =
                unsafe { core::ptr::read_volatile((page_address + byte_offset) as *const u8) };
            if actual != 0 {
                return Err(MemoryTestFailure::NotZero {
                    page_index,
                    byte_offset,
                    actual,
                });
            }
        }
    }

    for page_index in 0..allocation.page_count() {
        let page_address = allocation.start_address() + page_index * MEMORY_PAGE_BYTE_COUNT;
        for byte_offset in 0..MEMORY_PAGE_BYTE_COUNT {
            let expected = memory_test_pattern(page_index, byte_offset);
            // SAFETY: The kernel exclusively mapped this writable byte into this actor.
            unsafe {
                core::ptr::write_volatile((page_address + byte_offset) as *mut u8, expected);
            }
        }
    }

    // Verify after filling *all* pages, catching accidental physical-page aliasing.
    for page_index in 0..allocation.page_count() {
        let page_address = allocation.start_address() + page_index * MEMORY_PAGE_BYTE_COUNT;
        for byte_offset in 0..MEMORY_PAGE_BYTE_COUNT {
            let expected = memory_test_pattern(page_index, byte_offset);
            // SAFETY: The allocation remains mapped and readable until it is released.
            let actual =
                unsafe { core::ptr::read_volatile((page_address + byte_offset) as *const u8) };
            if actual != expected {
                return Err(MemoryTestFailure::Corrupted {
                    page_index,
                    byte_offset,
                    expected,
                    actual,
                });
            }
        }
    }
    Ok(())
}

/// Snapshot of free physical frames; the kernel census is not atomic across cores.
fn free_memory_frame_count() -> Result<u64, SystemCallStatus> {
    let statistics = observation::read_statistics()?;
    Ok(statistics[2])
}

/// A bounded formatting buffer; each command emits one record at a time.
struct Text<const N: usize> {
    bytes: [u8; N],
    length: usize,
}
impl<const N: usize> Text<N> {
    const fn new() -> Self {
        Self {
            bytes: [0; N],
            length: 0,
        }
    }

    fn as_str(&self) -> &str {
        self.bytes
            .get(..self.length)
            .and_then(|bytes| core::str::from_utf8(bytes).ok())
            .unwrap_or("")
    }
}
impl<const N: usize> Write for Text<N> {
    fn write_str(&mut self, text: &str) -> fmt::Result {
        let end = self.length.checked_add(text.len()).ok_or(fmt::Error)?;
        self.bytes
            .get_mut(self.length..end)
            .ok_or(fmt::Error)?
            .copy_from_slice(text.as_bytes());
        self.length = end;
        Ok(())
    }
}

/// Renders an address to a string so width and alignment specifiers apply to it.
fn address_text(packed: u64) -> Text<48> {
    let mut text = Text::new();
    write!(text, "{}", LocalAddress::from_packed(packed)).unwrap_or_else(|_| fail());
    text
}

/// Parses `<0.slot.generation>`, `0.slot.generation`, or `slot.generation`.
fn parse_address(word: &[u8]) -> Option<u64> {
    let text = core::str::from_utf8(word).ok()?;
    let text = text.strip_prefix('<').unwrap_or(text);
    let text = text.strip_suffix('>').unwrap_or(text);
    let mut parts = text.split('.');
    let first = parts.next()?;
    let second = parts.next()?;
    let (slot, generation) = if let Some(third) = parts.next() {
        if parts.next().is_some() || first.parse::<u32>().ok()? != 0 {
            return None;
        }
        (second.parse::<u32>().ok()?, third.parse::<u32>().ok()?)
    } else {
        (first.parse::<u32>().ok()?, second.parse::<u32>().ok()?)
    };
    Some(LocalAddress::new(slot, generation).to_packed())
}

/// Returns user text only when it is safe to echo back to the terminal.
fn printable(word: &[u8]) -> &str {
    if word.len() <= 64 && word.iter().all(u8::is_ascii_graphic) {
        core::str::from_utf8(word).unwrap_or("?")
    } else {
        "?"
    }
}

/// Percentage rounded down; zero when the total is zero.
fn percent(part: u64, total: u64) -> u64 {
    if total == 0 {
        0
    } else {
        u64::try_from(u128::from(part.min(total)) * 100 / u128::from(total)).unwrap_or(100)
    }
}

/// Renders a percentage, showing `<1%` when the part is nonzero but rounds to zero.
fn percent_text(part: u64, total: u64) -> Text<8> {
    let value = percent(part, total);
    let mut text = Text::new();
    if part > 0 && value == 0 {
        text.write_str("<1%").unwrap_or_else(|_| fail());
    } else {
        write!(text, "{value}%").unwrap_or_else(|_| fail());
    }
    text
}

/// Renders a colored `[█████░░░░░]` bar for `used` out of `total`.
fn bar(used: u64, total: u64) -> Text<192> {
    let (filled_glyph, empty_glyph) = if UNICODE_BARS {
        ("█", "░")
    } else {
        ("#", ".")
    };
    let value = percent(used, total);
    let color = if value >= CRITICAL_PERCENT {
        RED
    } else if value >= WARNING_PERCENT {
        YELLOW
    } else {
        GREEN
    };
    let mut filled = if total == 0 {
        0
    } else {
        let scaled = u128::from(used.min(total)) * u128::from(BAR_WIDTH) / u128::from(total);
        u64::try_from(scaled).unwrap_or(BAR_WIDTH).min(BAR_WIDTH)
    };
    // A nonzero value should always be visible.
    if used > 0 && filled == 0 {
        filled = 1;
    }
    let mut text = Text::new();
    write!(text, "{DIM}[{RESET}{color}").unwrap_or_else(|_| fail());
    for _ in 0..filled {
        text.write_str(filled_glyph).unwrap_or_else(|_| fail());
    }
    write!(text, "{RESET}{DIM}").unwrap_or_else(|_| fail());
    for _ in filled..BAR_WIDTH {
        text.write_str(empty_glyph).unwrap_or_else(|_| fail());
    }
    write!(text, "]{RESET}").unwrap_or_else(|_| fail());
    text
}

/// Splits a frame count into whole and tenth mebibytes.
fn mebibytes(frames: u64) -> (u64, u64) {
    (
        frames / FRAMES_PER_MEBIBYTE,
        (frames % FRAMES_PER_MEBIBYTE) * 10 / FRAMES_PER_MEBIBYTE,
    )
}

/// Display name and color for a census state number.
fn state_style(state: u8) -> (&'static str, &'static str) {
    match state {
        1 => ("Runnable", YELLOW),
        2 => ("Running", GREEN),
        3 => ("Waiting", CYAN),
        4 => ("Exiting", RED),
        _ => ("Unknown", DIM),
    }
}

/// Maps a typed state name to its census state number.
fn state_from_name(word: &[u8]) -> Option<u8> {
    if word.eq_ignore_ascii_case(b"runnable") {
        Some(1)
    } else if word.eq_ignore_ascii_case(b"running") {
        Some(2)
    } else if word.eq_ignore_ascii_case(b"waiting") {
        Some(3)
    } else if word.eq_ignore_ascii_case(b"exiting") {
        Some(4)
    } else {
        None
    }
}

/// One occupied actor slot copied out of the census (32 bytes).
#[derive(Clone, Copy)]
struct Entry {
    packed: u64,
    parent: u64,
    messages: u32,
    charged: u32,
    state: u8,
    has_parent: bool,
}
impl Entry {
    fn parent(&self) -> Option<u64> {
        if self.has_parent {
            Some(self.parent)
        } else {
            None
        }
    }
}

const EMPTY_ENTRY: Entry = Entry {
    packed: 0,
    parent: 0,
    messages: 0,
    charged: 0,
    state: 0,
    has_parent: false,
};

/// A bounded, non-atomic copy of the occupied slots.
struct Census {
    entries: [Entry; SNAPSHOT_LIMIT],
    count: usize,
    slots: u64,
    truncated: bool,
}
impl Census {
    const fn new() -> Self {
        Self {
            entries: [EMPTY_ENTRY; SNAPSHOT_LIMIT],
            count: 0,
            slots: 0,
            truncated: false,
        }
    }

    fn entries(&self) -> &[Entry] {
        self.entries.get(..self.count).unwrap_or(&[])
    }
}

/// How `actors` orders its rows.
#[derive(Clone, Copy)]
enum SortKey {
    Census,
    Messages,
    Bytes,
}

/// Index of the entry with this packed address.
fn index_of(entries: &[Entry], packed: u64) -> Option<usize> {
    entries.iter().position(|entry| entry.packed == packed)
}

/// Index of an entry's parent; `None` for roots and for orphans whose parent is gone.
fn parent_index(entries: &[Entry], index: usize) -> Option<usize> {
    let parent = entries.get(index)?.parent()?;
    index_of(entries, parent)
}

/// Whether a later entry shares this entry's parent.
fn has_later_sibling(entries: &[Entry], index: usize) -> bool {
    let group = parent_index(entries, index);
    (index + 1..entries.len()).any(|other| parent_index(entries, other) == group)
}

/// Number of ancestors above an entry (bounded, so a corrupt cycle cannot hang).
fn depth_of(entries: &[Entry], index: usize) -> usize {
    let mut depth = 0;
    let mut current = index;
    for _ in 0..entries.len() {
        match parent_index(entries, current) {
            Some(parent) => {
                depth += 1;
                current = parent;
            }
            None => break,
        }
    }
    depth
}

/// The ancestor `levels` steps above an entry (the entry itself when `levels` is zero).
fn ancestor(entries: &[Entry], index: usize, levels: usize) -> usize {
    let mut current = index;
    for _ in 0..levels {
        match parent_index(entries, current) {
            Some(parent) => current = parent,
            None => break,
        }
    }
    current
}

/// Next entry in depth-first order, found without any stack or scratch arrays.
fn next_in_tree(entries: &[Entry], index: usize) -> Option<usize> {
    let packed = entries.get(index)?.packed;
    if let Some(child) = entries
        .iter()
        .position(|entry| entry.parent() == Some(packed))
    {
        return Some(child);
    }
    let mut current = index;
    for _ in 0..entries.len() {
        let group = parent_index(entries, current);
        if let Some(sibling) =
            (current + 1..entries.len()).find(|&other| parent_index(entries, other) == group)
        {
            return Some(sibling);
        }
        current = group?;
    }
    None
}

/// Lowest mapped user-stack address, inferred from the two overflow faults
/// (four pages below the 0x7fffff1000 stack top).
const STACK_FLOOR: u64 = 0x7f_fffe_d000;

/// Reads the current stack pointer.
fn stack_pointer() -> u64 {
    let pointer: u64;
    // SAFETY: Reading `sp` has no side effects and touches no memory.
    unsafe {
        core::arch::asm!("mov {}, sp", out(reg) pointer, options(nomem, nostack, preserves_flags));
    }
    pointer
}

/// Buffers formatted output and flushes it to the console one chunk at a time.
struct Sink<'terminal> {
    terminal: &'terminal mut Terminal,
    bytes: [u8; PRINT_CHUNK],
    length: usize,
}
impl Sink<'_> {
    fn flush(&mut self) {
        let length = self.length;
        self.length = 0;
        if length != 0 {
            self.terminal.write(self.bytes.get(..length).unwrap_or(&[]));
        }
    }
}
impl Write for Sink<'_> {
    fn write_str(&mut self, text: &str) -> fmt::Result {
        for &byte in text.as_bytes() {
            if self.length == PRINT_CHUNK {
                self.flush();
            }
            *self.bytes.get_mut(self.length).ok_or(fmt::Error)? = byte;
            self.length += 1;
        }
        Ok(())
    }
}

/// Bounded output pipeline with ordered acknowledgements and pulled input.
struct Terminal {
    driver: LocalAddress,
    /// Boot-bound directory service, resolved lazily through the console driver.
    filesystem_address: Option<LocalAddress>,
    /// Root filesystem handle held for the lifetime of this terminal session.
    filesystem_root_handle: Option<FileSystemHandle>,
    /// Canonical working directory owned by this terminal session.
    working_directory: directory_path::DirectoryPath,
    correlation: u64,
    /// Correlations in send order; payload storage belongs to the message pool.
    outstanding_output: [u64; MAXIMUM_OUTSTANDING_OUTPUT_COUNT],
    /// Initialized entries at the beginning of `outstanding_output`.
    outstanding_output_count: usize,
    allocation: Option<Allocation>,
}
impl Terminal {
    /// Sends one request, retrying only admission failures before it is accepted.
    #[inline(never)]
    fn request(&mut self, tag: Tag, payload: &[u8]) -> u64 {
        self.flush_output();
        self.correlation = self.correlation.checked_add(1).unwrap_or_else(|| fail());
        loop {
            match inbox::tell_actor(self.driver, tag, self.correlation, payload) {
                Ok(()) => return self.correlation,
                Err(error) if error.status == SystemCallStatus::BudgetExhausted => {
                    core::hint::spin_loop();
                }
                Err(_) => fail(),
            }
        }
    }

    /// Waits for the driver's response to one specific request.
    fn response<'buffer>(
        &self,
        tag: Tag,
        correlation: u64,
        record: &'buffer mut [u8],
    ) -> &'buffer [u8] {
        inbox::await_matching(
            MessageSelector {
                sender: Some(self.driver),
                tag: Some(tag),
                correlation: Some(correlation),
            },
            record,
        )
        .unwrap_or_else(|_| fail())
        .payload
    }

    /// Retires the oldest admitted write using authenticated correlation matching.
    fn complete_oldest_output(&mut self) {
        if self.outstanding_output_count == 0 {
            return;
        }
        let correlation = *self.outstanding_output.first().unwrap_or_else(|| fail());
        let mut record = [0_u8; 24];
        if !self
            .response(console::WRITTEN_TAG, correlation, &mut record)
            .is_empty()
        {
            fail();
        }
        self.outstanding_output
            .copy_within(1..self.outstanding_output_count, 0);
        self.outstanding_output_count -= 1;
    }

    /// Completes all physical output before a control request or input wait.
    fn flush_output(&mut self) {
        while self.outstanding_output_count != 0 {
            self.complete_oldest_output();
        }
    }

    /// Admits at most four ordered chunks without waiting after every send.
    ///
    /// Admission pressure retires an existing write before retrying. A successful
    /// admission is never replayed, and only correlations occupy terminal storage.
    /// `request` drains the pipeline before asking for input or service bindings.
    #[inline(never)]
    fn write(&mut self, bytes: &[u8]) {
        for chunk in bytes.chunks(console::MAXIMUM_OUTPUT_BYTE_COUNT) {
            if self.outstanding_output_count == MAXIMUM_OUTSTANDING_OUTPUT_COUNT {
                self.complete_oldest_output();
            }
            self.correlation = self.correlation.checked_add(1).unwrap_or_else(|| fail());
            let correlation = self.correlation;
            loop {
                match inbox::tell_actor(self.driver, console::WRITE_TAG, correlation, chunk) {
                    Ok(()) => break,
                    Err(error) if error.status == SystemCallStatus::BudgetExhausted => {
                        if self.outstanding_output_count == 0 {
                            core::hint::spin_loop();
                        } else {
                            self.complete_oldest_output();
                        }
                    }
                    Err(_) => fail(),
                }
            }
            *self
                .outstanding_output
                .get_mut(self.outstanding_output_count)
                .unwrap_or_else(|| fail()) = correlation;
            self.outstanding_output_count += 1;
        }
    }

    /// Formats straight to the console in small chunks.
    ///
    /// Never inlined, and it buffers only `PRINT_CHUNK` bytes: the EL0 stack is tiny.
    #[inline(never)]
    fn print(&mut self, arguments: fmt::Arguments<'_>) {
        let mut sink = Sink {
            terminal: self,
            bytes: [0; PRINT_CHUNK],
            length: 0,
        };
        sink.write_fmt(arguments).unwrap_or_else(|_| fail());
        sink.flush();
    }

    /// Prints a section heading.
    fn heading(&mut self, title: &str) {
        self.print(format_args!("{BOLD}{CYAN} {title}{RESET}\n"));
    }

    /// Prints a one-line usage hint.
    fn usage(&mut self, text: &str) {
        self.print(format_args!("{YELLOW}Usage:{RESET} {text}\n"));
    }

    /// Requests the next nonempty input chunk.
    #[inline(never)]
    fn read(&mut self, input: &mut [u8; console::MAXIMUM_INPUT_BYTE_COUNT]) -> usize {
        let correlation = self.request(console::READ_TAG, &[]);
        let mut record = [0_u8; 24 + console::MAXIMUM_INPUT_BYTE_COUNT];
        let bytes = self.response(console::INPUT_TAG, correlation, &mut record);
        input
            .get_mut(..bytes.len())
            .unwrap_or_else(|| fail())
            .copy_from_slice(bytes);
        bytes.len()
    }

    /// Fills `census` with occupied slots; not one atomic view of the table.
    #[inline(never)]
    fn collect(&mut self, census: &mut Census) -> bool {
        let mut slot = 0;
        loop {
            let (record, capacity) = match observation::read_census(slot) {
                Ok(record) => record,
                Err(error) => {
                    self.print(format_args!("{RED}Cannot read census: {error:?}{RESET}\n"));
                    return false;
                }
            };
            let [_, packed, state, has_parent, parent, messages, charged, _] = record;
            if state != State::Free as u64 {
                let entry = Entry {
                    packed,
                    parent,
                    messages: u32::try_from(messages).unwrap_or(u32::MAX),
                    charged: u32::try_from(charged).unwrap_or(u32::MAX),
                    state: u8::try_from(state).unwrap_or(u8::MAX),
                    has_parent: has_parent != 0,
                };
                if let Some(target) = census.entries.get_mut(census.count) {
                    *target = entry;
                    census.count += 1;
                } else {
                    census.truncated = true;
                }
            }
            slot += 1;
            if slot >= capacity {
                break;
            }
        }
        census.slots = u64::try_from(slot).unwrap_or(0);
        true
    }

    /// Lists occupied slots, optionally filtered by state and sorted.
    #[inline(never)]
    fn actors(&mut self, filter: Option<u8>, sort: SortKey) {
        let mut census = Census::new();
        if !self.collect(&mut census) {
            return;
        }
        if let Some(slice) = census.entries.get_mut(..census.count) {
            match sort {
                SortKey::Census => {}
                SortKey::Messages => slice.sort_unstable_by_key(|entry| Reverse(entry.messages)),
                SortKey::Bytes => slice.sort_unstable_by_key(|entry| Reverse(entry.charged)),
            }
        }

        self.print(format_args!(
            "{BOLD}{CYAN} {:<12} {:<10} {:<12} {:<10} {:<8}{RESET}\n",
            "ADDRESS", "STATE", "PARENT", "MESSAGES", "BYTES"
        ));
        self.print(format_args!(
            "{DIM} {:-<12} {:-<10} {:-<12} {:-<10} {:-<8}{RESET}\n",
            "", "", "", "", ""
        ));

        let mut shown = 0_u32;
        for entry in census.entries() {
            if filter.is_some_and(|state| state != entry.state) {
                continue;
            }

            let (name, color) = state_style(entry.state);
            let own = address_text(entry.packed);
            let (parent_text, parent_color) = match entry.parent() {
                Some(parent) => (address_text(parent), ""),
                None => {
                    let mut none = Text::<48>::new();
                    none.write_str("-").unwrap_or_else(|_| fail());
                    (none, DIM)
                }
            };

            self.print(format_args!(
                " {:<12} {color}{name:<10}{RESET} {parent_color}{:<12}{RESET} {:<10} {:<8}\n",
                own.as_str(),
                parent_text.as_str(),
                entry.messages,
                entry.charged,
            ));

            shown += 1;
        }

        let total = census.count;
        let slots = census.slots;
        self.print(format_args!(
            "{DIM} {shown} of {total} actor(s) shown; {slots} slots scanned (not an atomic snapshot){RESET}\n"
        ));
        if census.truncated {
            self.print(format_args!(
                "{YELLOW} Only the first {SNAPSHOT_LIMIT} actors are kept; the census was truncated.{RESET}\n"
            ));
        }
    }

    /// Shows one actor in detail, including its children.
    #[inline(never)]
    fn actor(&mut self, target: u64) {
        let mut census = Census::new();
        if !self.collect(&mut census) {
            return;
        }
        let wanted = address_text(target);
        let Some(entry) = census.entries().iter().find(|entry| entry.packed == target) else {
            self.print(format_args!(
                "{RED}No live actor at {}{RESET}\n",
                wanted.as_str()
            ));
            return;
        };
        let (name, color) = state_style(entry.state);
        let parent = entry.parent().map(address_text);
        let mut children = Text::<256>::new();
        let mut any = false;
        for child in census
            .entries()
            .iter()
            .filter(|child| child.parent() == Some(entry.packed))
        {
            let text = address_text(child.packed);
            let separator = if any { " " } else { "" };
            if write!(children, "{separator}{}", text.as_str()).is_err() {
                children.write_str("...").ok();
                break;
            }
            any = true;
        }
        self.print(format_args!(
            "{BOLD}{CYAN} Actor {}{RESET}\n",
            wanted.as_str()
        ));
        self.print(format_args!("   {:<9} {color}{name}{RESET}\n", "State"));
        self.print(format_args!(
            "   {:<9} {}\n",
            "Parent",
            parent.as_ref().map_or("none", |text| text.as_str())
        ));
        self.print(format_args!(
            "   {:<9} {} message(s), {} byte(s) charged\n",
            "Inbox", entry.messages, entry.charged
        ));
        self.print(format_args!(
            "   {:<9} {}\n",
            "Children",
            if any { children.as_str() } else { "none" }
        ));
    }

    /// Draws the parent/child hierarchy with box-drawing characters.
    ///
    /// Walks the snapshot depth-first without a stack: a handful of locals only.
    #[inline(never)]
    fn tree(&mut self) {
        let mut census = Census::new();
        if !self.collect(&mut census) {
            return;
        }
        let entries = census.entries();
        self.heading("Actor tree");

        let mut next = (0..entries.len()).find(|&index| parent_index(entries, index).is_none());
        let mut visited = 0_usize;
        while let Some(index) = next {
            visited += 1;
            let Some(entry) = entries.get(index) else {
                break;
            };
            let depth = depth_of(entries, index);

            let mut prefix = Text::<96>::new();
            for level in 1..depth {
                let line = ancestor(entries, index, depth - level);
                prefix
                    .write_str(if has_later_sibling(entries, line) {
                        "│  "
                    } else {
                        "   "
                    })
                    .ok();
            }
            if depth > 0 {
                prefix
                    .write_str(if has_later_sibling(entries, index) {
                        "├─ "
                    } else {
                        "└─ "
                    })
                    .ok();
            }

            let (name, color) = state_style(entry.state);
            let own = address_text(entry.packed);
            let mut note = Text::<64>::new();
            if depth == 0 {
                if let Some(parent) = entry.parent() {
                    let gone = address_text(parent);
                    write!(note, " (parent {} gone)", gone.as_str()).ok();
                }
            }
            self.print(format_args!(
                " {}{color}{}{RESET} {DIM}{name}, {} message(s){}{RESET}\n",
                prefix.as_str(),
                own.as_str(),
                entry.messages,
                note.as_str(),
            ));

            next = if visited < entries.len() {
                next_in_tree(entries, index)
            } else {
                None
            };
        }

        let count = census.count;
        self.print(format_args!("{DIM} {count} actor(s){RESET}\n"));
        if census.truncated {
            self.print(format_args!(
                "{YELLOW} Only the first {SNAPSHOT_LIMIT} actors are kept; the census was truncated.{RESET}\n"
            ));
        }
    }

    /// Displays explicitly named counters in their documented units.
    #[inline(never)]
    fn statistics(&mut self) {
        match observation::read_statistics() {
            Ok(
                [
                    _,
                    total,
                    free,
                    capacity,
                    allocated,
                    charged,
                    cores,
                    slots,
                    occupied,
                    waiting,
                ],
            ) => {
                let used = total.saturating_sub(free);
                let (free_whole, free_tenth) = mebibytes(free);
                let (total_whole, total_tenth) = mebibytes(total);

                self.heading("Memory");
                self.print(format_args!(
                    "   {:<9} {} {:>4} used   {free} / {total} frames free ({free_whole}.{free_tenth} of {total_whole}.{total_tenth} MiB)\n\n",
                    "Frames",
                    bar(used, total).as_str(),
                    percent_text(used, total).as_str(),
                ));

                self.heading("Messaging");
                self.print(format_args!(
                    "   {:<9} {} {:>4} used   {allocated} / {capacity} messages\n",
                    "Pool",
                    bar(allocated, capacity).as_str(),
                    percent_text(allocated, capacity).as_str(),
                ));
                self.print(format_args!("   {:<9} {charged} bytes\n\n", "Charged"));

                self.heading("Actors");
                self.print(format_args!(
                    "   {:<9} {} {:>4} used   {occupied} / {slots} slots\n",
                    "Slots",
                    bar(occupied, slots).as_str(),
                    percent_text(occupied, slots).as_str(),
                ));
                self.print(format_args!("   {:<9} {waiting}\n\n", "Waiting"));

                self.heading("System");
                self.print(format_args!("   {:<9} {cores} online\n", "Cores"));
            }
            Err(error) => {
                self.print(format_args!(
                    "{RED}Cannot read statistics: {error:?}{RESET}\n"
                ));
            }
        }
    }

    /// Validates zero-filling, writes, reads, and physical-frame reclamation.
    ///
    /// A one-page warm-up creates the persistent intermediate translation tables
    /// before the free-frame baseline is sampled. It is released immediately.
    #[inline(never)]
    fn memory_test(&mut self, requested_kibibytes: usize) {
        if self.allocation.is_some() {
            self.print(format_args!(
                "{YELLOW}Release the existing allocation first.{RESET}\n"
            ));
            return;
        }

        let Some(page_count) = requested_kibibytes.checked_add(3).map(|size| size / 4) else {
            self.usage("memory test [KiB]");
            return;
        };
        if page_count == 0 || page_count > 256 {
            self.print(format_args!(
                "{YELLOW}Memory tests must request 1 to 1024 KiB.{RESET}\n"
            ));
            return;
        }

        // A first mapping allocates page-table frames that stay in the address
        // space after release. Account for those separately from test pages.
        let warmup = match allocator::allocate_pages(1) {
            Ok(allocation) => allocation,
            Err(status) => {
                self.print(format_args!(
                    "{RED}Memory test warm-up failed: {status:?}{RESET}\n"
                ));
                return;
            }
        };
        if let Err(status) = allocator::release_pages(warmup) {
            self.allocation = Some(warmup);
            self.print(format_args!(
                "{RED}Memory test warm-up release failed: {status:?}{RESET}\n"
            ));
            return;
        }

        let before = match free_memory_frame_count() {
            Ok(count) => count,
            Err(status) => {
                self.print(format_args!(
                    "{RED}Cannot read baseline frame statistics: {status:?}{RESET}\n"
                ));
                return;
            }
        };
        let allocation = match allocator::allocate_pages(page_count) {
            Ok(allocation) => allocation,
            Err(status) => {
                self.print(format_args!(
                    "{RED}Memory test allocation failed: {status:?}{RESET}\n"
                ));
                return;
            }
        };

        let during = free_memory_frame_count();
        let verification = verify_memory_allocation(allocation);
        let release = allocator::release_pages(allocation);
        if release.is_err() {
            // Preserve ownership so `memory release` can retry the operation.
            self.allocation = Some(allocation);
        }
        let after = free_memory_frame_count();

        self.report_memory_test(allocation, before, during, after, verification, release);
    }

    /// Prints content-verification, release, and independently sampled allocator outcomes.
    #[inline(never)]
    fn report_memory_test(
        &mut self,
        allocation: Allocation,
        frames_before: u64,
        frames_during: Result<u64, SystemCallStatus>,
        frames_after: Result<u64, SystemCallStatus>,
        verification: Result<(), MemoryTestFailure>,
        release: Result<(), SystemCallStatus>,
    ) {
        self.print(format_args!(
            "{BOLD}{CYAN} Memory test: {} KiB ({} pages){RESET}\n",
            allocation.page_count() * 4,
            allocation.page_count(),
        ));
        match verification {
            Ok(()) => self.print(format_args!(
                "   {GREEN}PASS{RESET}  Zero-fill and full-page write/read verification\n"
            )),
            Err(error) => self.print(format_args!(
                "   {RED}FAIL{RESET}  Memory verification: {error:?}\n"
            )),
        }
        match release {
            Ok(()) => self.print(format_args!(
                "   {GREEN}PASS{RESET}  Page release accepted\n"
            )),
            Err(status) => self.print(format_args!(
                "   {RED}FAIL{RESET}  Page release rejected: {status:?}\n"
            )),
        }

        let frame_accounting_passed = match (frames_during, frames_after) {
            (Ok(during_count), Ok(after_count)) => {
                self.print(format_args!(
                    "   Frames: {frames_before} before, {during_count} allocated, {after_count} after\n"
                ));
                let expected_during = frames_before.checked_sub(allocation.page_count() as u64);
                let correct = expected_during == Some(during_count) && after_count == frames_before;
                if !correct {
                    self.print(format_args!(
                        "   {YELLOW}Frame count differs (statistics are non-atomic).{RESET}\n"
                    ));
                }
                correct
            }
            (during_count, after_count) => {
                self.print(format_args!(
                    "   {RED}FAIL{RESET}  Cannot read frame statistics: {during_count:?}, {after_count:?}\n"
                ));
                false
            }
        };

        if verification.is_ok() && release.is_ok() && frame_accounting_passed {
            self.print(format_args!("{GREEN} Memory test passed.{RESET}\n"));
        } else {
            self.print(format_args!("{RED} Memory test failed.{RESET}\n"));
        }
    }

    /// Allocates or releases one kernel-backed memory region owned by this actor.
    #[inline(never)]
    fn memory_command(&mut self, arguments: &[&[u8]]) {
        match arguments {
            [operation] if operation.eq_ignore_ascii_case(b"status") => match self.allocation {
                Some(allocation) => self.print(format_args!(
                    "   Terminal owns {} KiB ({} page(s)) at {:#x}\n",
                    allocation.page_count() * 4,
                    allocation.page_count(),
                    allocation.start_address(),
                )),
                None => self.print(format_args!("   Terminal has no dynamic allocation.\n")),
            },
            [operation] if operation.eq_ignore_ascii_case(b"test") => {
                self.memory_test(DEFAULT_MEMORY_TEST_KIBIBYTES);
            }
            [operation, requested_size] if operation.eq_ignore_ascii_case(b"test") => {
                let Some(kibibytes) = core::str::from_utf8(requested_size)
                    .ok()
                    .and_then(|text| text.parse::<usize>().ok())
                    .filter(|size| *size > 0)
                else {
                    self.usage("memory test [KiB]");
                    return;
                };
                self.memory_test(kibibytes);
            }
            [operation] if operation.eq_ignore_ascii_case(b"release") => {
                let Some(allocation) = self.allocation else {
                    self.print(format_args!("{YELLOW}Nothing to release.{RESET}\n"));
                    return;
                };
                match allocator::release_pages(allocation) {
                    Ok(()) => {
                        self.allocation = None;
                        self.print(format_args!(
                            "{GREEN}Released {} KiB.{RESET}\n",
                            allocation.page_count() * 4,
                        ));
                    }
                    Err(status) => self.print(format_args!(
                        "{RED}Memory release failed: {status:?}{RESET}\n"
                    )),
                }
            }
            [operation, requested_size] if operation.eq_ignore_ascii_case(b"allocate") => {
                if self.allocation.is_some() {
                    self.print(format_args!(
                        "{YELLOW}Release the existing allocation first.{RESET}\n"
                    ));
                    return;
                }
                let Some(kibibytes) = core::str::from_utf8(requested_size)
                    .ok()
                    .and_then(|text| text.parse::<usize>().ok())
                    .filter(|size| *size > 0)
                else {
                    self.usage("memory allocate <KiB>");
                    return;
                };
                let Some(page_count) = kibibytes.checked_add(3).map(|bytes| bytes / 4) else {
                    self.usage("memory allocate <KiB>");
                    return;
                };
                match allocator::allocate_pages(page_count) {
                    Ok(allocation) => {
                        self.allocation = Some(allocation);
                        self.print(format_args!(
                            "{GREEN}Allocated {} KiB ({} page(s)) at {:#x}.{RESET}\n",
                            allocation.page_count() * 4,
                            allocation.page_count(),
                            allocation.start_address(),
                        ));
                    }
                    Err(status) => self.print(format_args!(
                        "{RED}Memory allocation failed: {status:?}{RESET}\n"
                    )),
                }
            }
            _ => self.usage(
                "memory allocate <KiB> | memory test [KiB] | memory status | memory release",
            ),
        }
    }

    /// Shows how much stack is left at this call depth.
    #[inline(never)]
    fn stack(&mut self) {
        let pointer = stack_pointer();
        let remaining = pointer.saturating_sub(STACK_FLOOR);
        self.heading("Stack");
        self.print(format_args!("   {:<9} {pointer:#x}\n", "Pointer"));
        self.print(format_args!(
            "   {:<9} {STACK_FLOOR:#x} {DIM}(inferred from overflow faults){RESET}\n",
            "Floor"
        ));
        self.print(format_args!(
            "   {:<9} {remaining} bytes below this call depth\n",
            "Left"
        ));
    }

    /// Prints the terminal's own version.
    #[inline(never)]
    fn version(&mut self) {
        self.print(format_args!(
            "{BOLD}{CYAN}Terminal{RESET} {} {DIM}(EL0, aarch64){RESET}\n",
            env!("CARGO_PKG_VERSION")
        ));
    }

    /// Streams the command list through one buffer, flushing full payloads and the tail.
    #[inline(never)]
    fn help(&mut self) {
        let mut sink = Sink {
            terminal: self,
            bytes: [0; PRINT_CHUNK],
            length: 0,
        };
        sink.write_fmt(format_args!("{BOLD}{CYAN} Commands{RESET}\n"))
            .unwrap_or_else(|_| fail());
        for (usage, description) in [
            ("help", "Show available commands"),
            (
                "list-directory [-la] [path]",
                "ld: list children of the working directory or path",
            ),
            (
                "make-directory <path>",
                "md: create one directory in memory",
            ),
            (
                "change-directory [path]",
                "cd: change directory (default /)",
            ),
            ("working-directory", "wd: show the current directory"),
            (
                "",
                "ld: -l/--long, -a/--all, -d/--directories (show target)",
            ),
            ("", "Relative paths, . and .. supported; memory only"),
            (
                "actors [state] [sort]",
                "List actors, optionally filtered and sorted",
            ),
            ("", "state: runnable|running|waiting|exiting"),
            ("", "sort:  messages|bytes"),
            (
                "actor <address>",
                "Show one actor in detail, e.g. actor <0.2.1>",
            ),
            ("tree", "Show the actor hierarchy"),
            (
                "memory allocate <KiB>",
                "Allocate kernel-backed memory for Terminal",
            ),
            ("memory status", "Show Terminal's current allocation"),
            ("memory test [KiB]", "Verify mapped pages and reclamation"),
            ("memory release", "Release Terminal's allocation"),
            ("stack", "Show remaining stack space"),
            ("statistics", "Show memory, messaging, and actor counters"),
            ("version", "Show the terminal version"),
            ("clear", "Clear the screen"),
        ] {
            sink.write_fmt(format_args!("   {BOLD}{usage:<24}{RESET}{description}\n"))
                .unwrap_or_else(|_| fail());
        }
        sink.write_fmt(format_args!("{DIM}   Tab completes command names; green = known, red = unknown.\n   aliases: ? = help, ld = list-directory, md = make-directory, cd = change-directory, wd = working-directory{RESET}\n"))
            .unwrap_or_else(|_| fail());
        sink.flush();
    }

    /// Reports and returns whether a command that takes no arguments received any.
    fn rejects_arguments(&mut self, name: &str, arguments: &[&[u8]]) -> bool {
        if arguments.is_empty() {
            return false;
        }
        self.print(format_args!(
            "{RED}{name} takes no arguments.{RESET} Type {BOLD}help{RESET}.\n"
        ));
        true
    }

    /// Parses the optional filter and sort words of `actors`.
    #[inline(never)]
    fn actors_command(&mut self, arguments: &[&[u8]]) {
        let mut filter = None;
        let mut sort = SortKey::Census;
        for &argument in arguments {
            if let Some(state) = state_from_name(argument) {
                filter = Some(state);
            } else if argument.eq_ignore_ascii_case(b"messages") {
                sort = SortKey::Messages;
            } else if argument.eq_ignore_ascii_case(b"bytes") {
                sort = SortKey::Bytes;
            } else {
                self.usage("actors [runnable|running|waiting|exiting] [messages|bytes]");
                return;
            }
        }
        self.actors(filter, sort);
    }

    /// Parses the address argument of `actor`.
    #[inline(never)]
    fn actor_command(&mut self, arguments: &[&[u8]]) {
        let [argument] = arguments else {
            self.usage("actor <address>   e.g. actor <0.2.1>");
            return;
        };
        match parse_address(argument) {
            Some(target) => self.actor(target),
            None => self.print(format_args!(
                "{RED}'{}' is not an address.{RESET} Use the form <0.2.1>.\n",
                printable(argument)
            )),
        }
    }

    /// Splits a line into words and runs the matching command.
    #[inline(never)]
    fn execute(&mut self, line: &[u8]) {
        let empty: &[u8] = &[];
        let mut words = [empty; MAXIMUM_WORDS];
        let mut count = 0_usize;
        for word in line
            .split(u8::is_ascii_whitespace)
            .filter(|word| !word.is_empty())
        {
            if let Some(slot) = words.get_mut(count) {
                *slot = word;
                count += 1;
            } else {
                self.print(format_args!(
                    "{RED}Too many arguments.{RESET} Type {BOLD}help{RESET}.\n"
                ));
                return;
            }
        }
        let Some((&name, arguments)) = words.get(..count).and_then(<[_]>::split_first) else {
            return;
        };
        match name {
            b"help" | b"?" => {
                if !self.rejects_arguments("help", arguments) {
                    self.help();
                }
            }
            b"list-directory" | b"ld" => self.directory_command(false, arguments),
            b"make-directory" | b"md" => self.directory_command(true, arguments),
            b"change-directory" | b"cd" => self.change_directory(arguments),
            b"working-directory" | b"wd" => self.show_working_directory(arguments),
            b"actors" => self.actors_command(arguments),
            b"actor" => self.actor_command(arguments),
            b"tree" => {
                if !self.rejects_arguments("tree", arguments) {
                    self.tree();
                }
            }
            b"memory" => self.memory_command(arguments),
            b"statistics" => {
                if !self.rejects_arguments("statistics", arguments) {
                    self.statistics();
                }
            }
            b"stack" => {
                if !self.rejects_arguments("stack", arguments) {
                    self.stack();
                }
            }
            b"version" => {
                if !self.rejects_arguments("version", arguments) {
                    self.version();
                }
            }
            b"clear" => {
                if !self.rejects_arguments("clear", arguments) {
                    self.write(CLEAR_SCREEN);
                }
            }
            _ => self.print(format_args!(
                "{RED}Unknown command '{}'.{RESET} Type {BOLD}help{RESET}.\n",
                printable(name)
            )),
        }
    }
}

/// Uses its boot-authenticated driver binding, then handles interactive input.
#[unsafe(no_mangle)]
pub extern "C" fn _start() -> ! {
    let driver = find_actor_by_name(Name::CONSOLE.as_str()).unwrap_or_else(|_| fail());
    let mut terminal = Terminal {
        driver,
        filesystem_address: None,
        filesystem_root_handle: None,
        working_directory: directory_path::DirectoryPath::root(),
        correlation: 0,
        outstanding_output: [0; MAXIMUM_OUTSTANDING_OUTPUT_COUNT],
        outstanding_output_count: 0,
        allocation: None,
    };
    #[cfg(feature = "observation-tests")]
    {
        validation::run();
        terminal.write(b"Observation copy tests passed\n");
    }
    terminal.write(b"\n\x1b[1;36mTerminal\x1b[0m\n\x1b[2mType help for commands.\x1b[0m\n");
    terminal.show_prompt();
    let mut editor = LineEditor::new();
    let mut input = [0_u8; console::MAXIMUM_INPUT_BYTE_COUNT];
    loop {
        let length = terminal.read(&mut input);
        for &byte in input.get(..length).unwrap_or_else(|| fail()) {
            match editor.feed(byte) {
                InputEvent::Ignored => {}
                InputEvent::Echo(_) | InputEvent::Erase => {
                    terminal.begin_update();
                    terminal.redraw_line(editor.line());
                    terminal.right_prompt();
                    terminal.end_update();
                }
                InputEvent::Complete => {
                    terminal.begin_update();
                    terminal.complete_line(&mut editor);
                    terminal.right_prompt();
                    terminal.end_update();
                }
                InputEvent::Cancel => {
                    terminal.write(b"^C\n");
                    terminal.show_prompt();
                }
                InputEvent::TooLong => {
                    terminal.write(b"\n\x1b[31mCommand too long; discarded.\x1b[0m\n");
                    terminal.show_prompt();
                    editor.reset();
                }
                InputEvent::Submit => {
                    terminal.write(b"\n");
                    terminal.execute(editor.line());
                    editor.reset();
                    terminal.show_prompt();
                }
            }
        }
    }
}

/// Reports a terminal panic as an actor-local fault.
#[panic_handler]
fn panic(_information: &core::panic::PanicInfo<'_>) -> ! {
    fail()
}
