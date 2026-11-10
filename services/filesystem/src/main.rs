//! EL0 filesystem actor with private, volatile directory storage.
//!
//! Boot supplies the sole client's authenticated address in x0. Only that actor
//! may submit requests in this first slice; everyone else is ignored. Each request
//! is handled exactly once and its reply is built before sending, so retrying a
//! send under admission pressure never repeats a creation.
//! The actor parks in AwaitInbox when idle and has no device or kernel mappings.

#![no_std]
#![no_main]

use foundation::{
    address::LocalAddress,
    filesystem::{self, Status},
    messaging::MAXIMUM_RECORD_BYTE_COUNT,
    system_call_status::SystemCallStatus,
    tag::Tag,
};
use runtime::inbox::{self, MessageSelector};

/// Most directories the service can hold, excluding the implicit root.
const CAPACITY: usize = 64;
const _: () = assert!(CAPACITY < u16::MAX as usize, "Cursors must fit in a u16");

/// Enters an actor-local fault if the service cannot continue safely.
fn fail() -> ! {
    // SAFETY: This deliberate EL0 breakpoint invokes the kernel's actor fault path.
    unsafe { core::arch::asm!("brk #1", options(noreturn)) }
}

/// One stored directory: its full absolute path.
struct Directory {
    path: [u8; filesystem::MAXIMUM_PATH_BYTE_COUNT],
    length: usize,
}

impl Directory {
    const EMPTY: Self = Self {
        path: [0; filesystem::MAXIMUM_PATH_BYTE_COUNT],
        length: 0,
    };

    fn path(&self) -> &[u8] {
        self.path.get(..self.length).unwrap_or(&[])
    }
}

/// Splits an absolute path at its last separator into (parent, name).
/// The parent of a top-level directory is the root.
fn split_path(path: &[u8]) -> (&[u8], &[u8]) {
    match path.iter().rposition(|&byte| byte == b'/') {
        Some(0) | None => (b"/", path.get(1..).unwrap_or(&[])),
        Some(index) => (
            path.get(..index).unwrap_or(&[]),
            path.get(index + 1..).unwrap_or(&[]),
        ),
    }
}

/// Absolute, ASCII, no empty components, no `.` or `..`. The root is valid.
fn is_valid_path(path: &[u8]) -> bool {
    if path == b"/" {
        return true;
    }
    let Some(rest) = path.strip_prefix(b"/") else {
        return false;
    };
    rest.split(|&byte| byte == b'/').all(|component| {
        !component.is_empty()
            && component != b"."
            && component != b".."
            && component.iter().all(u8::is_ascii_graphic)
    })
}

/// The directory table. Entries are kept in creation order, which is the listing order.
struct Store {
    entries: [Directory; CAPACITY],
    count: usize,
}

impl Store {
    const fn new() -> Self {
        Self {
            entries: [Directory::EMPTY; CAPACITY],
            count: 0,
        }
    }

    fn stored(&self) -> &[Directory] {
        self.entries.get(..self.count).unwrap_or(&[])
    }

    fn contains(&self, path: &[u8]) -> bool {
        path == b"/" || self.stored().iter().any(|entry| entry.path() == path)
    }

    fn create(&mut self, path: &[u8]) -> Status {
        if !is_valid_path(path) || path == b"/" {
            return Status::InvalidRequest;
        }
        if self.contains(path) {
            return Status::AlreadyExists;
        }
        if !self.contains(split_path(path).0) {
            return Status::NotFound;
        }
        let Some(slot) = self.entries.get_mut(self.count) else {
            return Status::CapacityExhausted;
        };
        let Some(target) = slot.path.get_mut(..path.len()) else {
            return Status::InvalidRequest;
        };
        target.copy_from_slice(path);
        slot.length = path.len();
        self.count += 1;
        Status::Accepted
    }

    /// First child of `parent` stored at or after `cursor`: its index and name.
    fn next_child(&self, parent: &[u8], cursor: usize) -> Option<(usize, &[u8])> {
        self.stored()
            .iter()
            .enumerate()
            .skip(cursor)
            .find_map(|(index, entry)| {
                let (entry_parent, name) = split_path(entry.path());
                (entry_parent == parent).then_some((index, name))
            })
    }
}

/// The directory table lives in static storage; the EL0 stack is too small for it.
static mut STORE: Store = Store::new();

/// Hands out the table. Called once, by the only thread of this actor.
fn take_store() -> &'static mut Store {
    // SAFETY: This actor is single-threaded and calls this exactly once, so the
    // returned reference is the only one that ever exists.
    unsafe { &mut *(&raw mut STORE) }
}

/// Handles one request and builds its reply in `reply`; returns the reply length.
///
/// A listing cursor is the table index of the last child returned, plus one, so a
/// returned child always has a nonzero cursor and zero with an empty name ends it.
fn serve(store: &mut Store, tag: Tag, payload: &[u8], reply: &mut [u8]) -> Option<usize> {
    match tag {
        filesystem::CREATE_DIRECTORY_TAG => {
            let status = match filesystem::decode_create_request(payload) {
                Some(path) => store.create(path),
                None => Status::InvalidRequest,
            };
            filesystem::encode_response(status, 0, &[], reply)
        }
        filesystem::LIST_DIRECTORY_TAG => {
            let Some((cursor, path)) = filesystem::decode_list_request(payload) else {
                return filesystem::encode_response(Status::InvalidRequest, 0, &[], reply);
            };
            if !is_valid_path(path) {
                return filesystem::encode_response(Status::InvalidRequest, 0, &[], reply);
            }
            if !store.contains(path) {
                return filesystem::encode_response(Status::NotFound, 0, &[], reply);
            }
            match store.next_child(path, usize::from(cursor)) {
                Some((index, name)) => {
                    let next = u16::try_from(index + 1).unwrap_or_else(|_| fail());
                    filesystem::encode_response(Status::Accepted, next, name, reply)
                }
                None => filesystem::encode_response(Status::Accepted, 0, &[], reply),
            }
        }
        _ => filesystem::encode_response(Status::InvalidRequest, 0, &[], reply),
    }
}

/// Sends one built reply, retrying only admission failures.
fn send(client: LocalAddress, correlation: u64, reply: &[u8]) {
    loop {
        match inbox::tell_actor(client, filesystem::RESPONSE_TAG, correlation, reply) {
            Ok(()) => return,
            Err(error) if error.status == SystemCallStatus::BudgetExhausted => {
                core::hint::spin_loop();
            }
            Err(_) => fail(),
        }
    }
}

/// Serves the boot-selected client.
#[unsafe(no_mangle)]
pub extern "C" fn _start(client_identity: u64) -> ! {
    let client = LocalAddress::from_packed(client_identity);
    let store = take_store();
    // Sized for the largest record, so oversized requests are consumed, not stranded.
    let mut record = [0; MAXIMUM_RECORD_BYTE_COUNT];
    let mut reply = [0; filesystem::MAXIMUM_RESPONSE_BYTE_COUNT];

    loop {
        let message = inbox::await_matching(MessageSelector::default(), &mut record)
            .unwrap_or_else(|_| fail());

        if message.sender != client {
            continue;
        }
        let Some(length) = serve(store, message.tag, message.payload, &mut reply) else {
            fail();
        };
        send(
            message.sender,
            message.correlation,
            reply.get(..length).unwrap_or_else(|| fail()),
        );
    }
}

/// Converts a service panic into an isolated actor fault.
#[panic_handler]
fn panic(_information: &core::panic::PanicInfo<'_>) -> ! {
    fail()
}
