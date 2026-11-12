//! EL0 file system actor with private, volatile storage.
//!
//! Boot supplies the sole client's authenticated address in x0. Only that actor
//! may submit requests; everyone else is ignored. Each request is handled exactly
//! once and its reply is built before sending, so retrying a send under admission
//! pressure never repeats a creation or a write.
//! The actor parks in AwaitInbox when idle and has no device or kernel mappings.

#![no_std]
#![no_main]

use foundation::{
    address::{Address, LocalAddress},
    file_system::{AccessRights, FileSystem},
    file_system_protocol::{self as protocol, Status},
    messaging::MAXIMUM_RECORD_BYTE_COUNT,
    system_call_status::SystemCallStatus,
    tag::Tag,
};
use runtime::inbox::{self, MessageSelector};

/// Enters an actor-local fault if the service cannot continue safely.
fn fail() -> ! {
    // SAFETY: This deliberate EL0 breakpoint invokes the kernel's actor fault path.
    unsafe { core::arch::asm!("brk #1", options(noreturn)) }
}

/// The file system lives in static storage; the EL0 stack is too small for it.
static mut FILE_SYSTEM: FileSystem = FileSystem::new();

/// Hands out the file system. Called once, by the only thread of this actor.
fn take_file_system() -> &'static mut FileSystem {
    // SAFETY: This actor is single-threaded and calls this exactly once, so the
    // returned reference is the only one that ever exists.
    unsafe { &mut *(&raw mut FILE_SYSTEM) }
}

/// The reply to a request that could not be decoded or whose tag is unknown.
fn invalid_request(reply: &mut [u8]) -> Option<usize> {
    protocol::encode_response(Status::InvalidRequest, &[], reply)
}

/// Handles one request and builds its reply in `reply`; returns the reply length.
///
/// Returns `None` only when the file system reports an inconsistency or the reply
/// buffer is too small, and the caller faults the actor in both cases.
fn serve(
    file_system: &mut FileSystem,
    owner: Address,
    tag: Tag,
    payload: &[u8],
    reply: &mut [u8],
) -> Option<usize> {
    match tag {
        protocol::ATTACH_TAG => {
            if !payload.is_empty() {
                return invalid_request(reply);
            }
            let result = file_system.attach(owner, AccessRights::READ_WRITE);
            protocol::encode_handle_result(result, reply)
        }
        protocol::WALK_TAG => {
            let Some(request) = protocol::decode_walk_request(payload) else {
                return invalid_request(reply);
            };
            let result = file_system.walk(owner, request.handle, request.names());
            protocol::encode_handle_result(result, reply)
        }
        protocol::OPEN_TAG => {
            let Some(request) = protocol::decode_open_request(payload) else {
                return invalid_request(reply);
            };
            let result = file_system.open(owner, request.handle, request.mode);
            protocol::encode_unit_result(result, reply)
        }
        protocol::CREATE_TAG => {
            let Some(request) = protocol::decode_create_request(payload) else {
                return invalid_request(reply);
            };
            let result = file_system.create(owner, request.handle, request.name, request.kind);
            protocol::encode_handle_result(result, reply)
        }
        protocol::READ_TAG => {
            let Some(request) = protocol::decode_read_request(payload) else {
                return invalid_request(reply);
            };
            // The file system reads straight into the reply, after its status byte.
            let destination = protocol::read_response_body(reply, request.count)?;
            let result = file_system.read(owner, request.handle, request.offset, destination);
            protocol::encode_read_result(result, reply)
        }
        protocol::WRITE_TAG => {
            let Some(request) = protocol::decode_write_request(payload) else {
                return invalid_request(reply);
            };
            let result = file_system.write(owner, request.handle, request.offset, request.data);
            protocol::encode_count_result(result, reply)
        }
        protocol::READ_DIRECTORY_TAG => {
            let Some(request) = protocol::decode_read_directory_request(payload) else {
                return invalid_request(reply);
            };
            let result = file_system.read_directory(owner, request.handle, request.cursor);
            protocol::encode_directory_result(result, reply)
        }
        protocol::STATISTICS_TAG => {
            let Some(handle) = protocol::decode_handle_request(payload) else {
                return invalid_request(reply);
            };
            protocol::encode_statistics_result(file_system.statistics(owner, handle), reply)
        }
        protocol::REMOVE_TAG => {
            let Some(handle) = protocol::decode_handle_request(payload) else {
                return invalid_request(reply);
            };
            protocol::encode_unit_result(file_system.remove(owner, handle), reply)
        }
        protocol::CLUNK_TAG => {
            let Some(handle) = protocol::decode_handle_request(payload) else {
                return invalid_request(reply);
            };
            protocol::encode_unit_result(file_system.clunk(owner, handle), reply)
        }
        _ => invalid_request(reply),
    }
}

/// Sends one built reply, retrying only admission failures.
fn send(client: LocalAddress, correlation: u64, reply: &[u8]) {
    loop {
        match inbox::tell_actor(client, protocol::RESPONSE_TAG, correlation, reply) {
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
    let owner = Address::Local(client);
    let file_system = take_file_system();
    // Sized for the largest record, so oversized requests are consumed, not stranded.
    let mut record = [0; MAXIMUM_RECORD_BYTE_COUNT];
    let mut reply = [0; protocol::MAXIMUM_RESPONSE_BYTE_COUNT];

    loop {
        let message = inbox::await_matching(MessageSelector::default(), &mut record)
            .unwrap_or_else(|_| fail());

        if message.sender != client {
            continue;
        }
        let Some(length) = serve(file_system, owner, message.tag, message.payload, &mut reply)
        else {
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
