//! Normal boot's temporary EL1 console driver, EL0 terminal, and directory service.
//!
//! Input is polled on scheduled turns and returned only after a terminal read
//! request. Output uses acknowledgements; failed admissions retain a pending
//! response and never repeat physical output. Only the boot-selected terminal
//! can use the driver. Genesis remains a separate EL0 actor.
//! The driver also hands the terminal its boot-selected filesystem binding.
//! Directory operations themselves travel directly between the two EL0 actors.
//! This is bootstrap plumbing, not the future Steward supervision hierarchy.

use foundation::{
    actor::{Actor, Image},
    address::LocalAddress,
    configuration::{Configuration, Lineage, OverflowPolicy, Survival},
    console,
    device_tree::PowerStateCoordinationInterfaceConduit,
    name::Name,
    system_call_status::SystemCallStatus,
    tag::Tag,
};

use crate::{
    actors::{
        divider,
        inbox::MessageMetadata,
        table::{enqueue, find_actor_by_name},
    },
    architecture::aarch64::{early_console::EarlyConsole, processor},
    memory::address_translation::physical_to_virtual,
    messaging::{delivery::tell_actor, kernel_inbox},
    system_calls::actor_calls::current_address,
};

/// Independent EL0 directory service built by xtask.
static FILE_SYSTEM_PROGRAM: &[u8] = include_bytes!(env!("FILE_SYSTEM_ELF_PATH"));
/// Genesis ELF built independently from other services.
// static GENESIS_PROGRAM: &[u8] = include_bytes!(env!("GENESIS_ELF_PATH"));
/// Terminal ELF built independently from Genesis by xtask.
static TERMINAL_PROGRAM: &[u8] = include_bytes!(env!("TERMINAL_ELF_PATH"));

/// Parks the current actor forever.
fn halt() -> ! {
    loop {
        processor::wait_for_event();
    }
}

/// Starts normal userland and returns an error if a required actor cannot start.
///
/// # Errors
/// Returns an actor-creation failure description.
pub fn start(_conduit: Option<PowerStateCoordinationInterfaceConduit>) -> Result<(), &'static str> {
    let system_limit = divider::system_limit();
    let configuration = Configuration {
        resource_budget: system_limit.budget,
        overflow_policy: OverflowPolicy::DropNewest,
        lineage: Lineage::Untethered,
        survival: Survival::Detached,
    };

    let parent = LocalAddress::new(0, 0);

    let _ =
        enqueue(Actor::new(Name::CONSOLE, configuration, Image::Kernel(poll)).with_parent(parent));

    let _ = enqueue(
        Actor::new(Name::TERMINAL, configuration, Image::Elf(TERMINAL_PROGRAM)).with_parent(parent),
    );

    let _ = enqueue(
        Actor::new(
            Name::FILE_SYSTEM,
            configuration,
            Image::Elf(FILE_SYSTEM_PROGRAM),
        )
        .with_parent(parent),
    );

    Ok(())
}

/// A retained response prevents output duplication or input loss on backpressure.
struct Response {
    tag: Tag,
    correlation: u64,
    length: usize,
    bytes: [u8; console::MAXIMUM_INPUT_BYTE_COUNT],
}

impl Response {
    const fn empty(tag: Tag, correlation: u64) -> Self {
        Self {
            tag,
            correlation,
            length: 0,
            bytes: [0; console::MAXIMUM_INPUT_BYTE_COUNT],
        }
    }

    /// Acknowledges that one write request reached the physical console.
    const fn written(correlation: u64) -> Self {
        Self::empty(console::WRITTEN_TAG, correlation)
    }

    /// Carries received UART bytes for one read request.
    const fn input(correlation: u64) -> Self {
        Self::empty(console::INPUT_TAG, correlation)
    }

    /// Carries the filesystem address, or nothing when none is bound.
    fn filesystem_bound(correlation: u64, filesystem: Option<LocalAddress>) -> Self {
        let mut response = Self::empty(console::FILE_SYSTEM_BOUND_TAG, correlation);
        if let Some(address) = filesystem {
            let encoded = console::encode_address(address);
            if let Some(target) = response.bytes.get_mut(..encoded.len()) {
                target.copy_from_slice(&encoded);
                response.length = encoded.len();
            }
        }
        response
    }

    fn payload(&self) -> &[u8] {
        self.bytes.get(..self.length).unwrap_or(&[])
    }
}

/// All state owned by the console driver actor.
struct Driver {
    own: LocalAddress,
    terminal: LocalAddress,
    /// Correlation of the one outstanding read request, if any.
    requested_read: Option<u64>,
    /// Response whose delivery has not yet been admitted.
    pending: Option<Response>,
    /// Receive buffer for incoming requests.
    request: [u8; console::MAXIMUM_OUTPUT_BYTE_COUNT],
}

impl Driver {
    fn new(own: LocalAddress, terminal: LocalAddress) -> Self {
        Self {
            own,
            terminal,
            requested_read: None,
            pending: None,
            request: [0; console::MAXIMUM_OUTPUT_BYTE_COUNT],
        }
    }

    /// Tries to send the pending response. Backpressure keeps it for a later turn;
    /// any other failure is fatal for the driver.
    fn deliver_pending(&mut self) {
        let Some(response) = self.pending.as_ref() else {
            return;
        };
        let metadata = MessageMetadata {
            sender: self.own,
            tag: response.tag,
            correlation: response.correlation,
        };

        match tell_actor(self.terminal, metadata, response.payload()) {
            Ok(()) => self.pending = None,
            Err(SystemCallStatus::BudgetExhausted) => {}
            Err(_) => {
                crate::error!(EarlyConsole, "Console response failed");
                halt();
            }
        }
    }

    /// Takes at most one request from the inbox. Only the terminal is served.
    fn serve_request(&mut self) {
        let Some((metadata, length)) = kernel_inbox::receive(&mut self.request) else {
            return;
        };
        if metadata.sender != self.terminal {
            return;
        }

        match metadata.tag {
            console::WRITE_TAG => {
                EarlyConsole::write_bytes(self.request.get(..length).unwrap_or(&[]));
                self.pending = Some(Response::written(metadata.correlation));
            }
            console::READ_TAG if length == 0 && self.requested_read.is_none() => {
                self.requested_read = Some(metadata.correlation);
            }
            console::FILE_SYSTEM_ADDRESS_TAG if length == 0 => {
                let filesystem = find_actor_by_name(Name::FILE_SYSTEM);
                self.pending = Some(Response::filesystem_bound(metadata.correlation, filesystem));
            }
            _ => {}
        }
    }

    /// Answers an outstanding read request once the UART has bytes.
    fn collect_input(&mut self) {
        let Some(correlation) = self.requested_read else {
            return;
        };
        let base = physical_to_virtual(crate::platform::current::PL011_UART_PHYSICAL_BASE_ADDRESS);
        let mut response = Response::input(correlation);

        for slot in &mut response.bytes {
            // SAFETY: Boot maps this platform's PL011 as Device memory.
            // This actor is the sole receive owner; only it calls read_byte.
            let Some(received) = (unsafe { pl011_console::read_byte(base) }) else {
                break;
            };
            *slot = received;
            response.length += 1;
        }

        if response.length != 0 {
            self.requested_read = None;
            self.pending = Some(response);
        }
    }

    /// Sleeps when idle; spins briefly when a send is waiting for pool space.
    fn idle(&self) {
        if self.pending.is_none() {
            kernel_inbox::wait_for_request_or_interrupt();
        } else {
            core::hint::spin_loop();
        }
    }
}

/// Waits until the boot-spawned terminal exists.
fn await_terminal() -> LocalAddress {
    loop {
        if let Some(address) = find_actor_by_name(Name::TERMINAL) {
            return address;
        }
        processor::wait_for_event();
    }
}

/// Polls UART receive and authenticated output requests in bounded batches.
extern "C" fn poll() -> ! {
    let (_, packed) = current_address();
    let own = LocalAddress::from_packed(packed);
    let mut driver = Driver::new(own, await_terminal());

    loop {
        driver.deliver_pending();

        if driver.pending.is_none() {
            driver.serve_request();
        }

        if driver.pending.is_none() {
            driver.collect_input();
        }

        // Try the response immediately; admitted output is never replayed.
        driver.deliver_pending();
        driver.idle();
    }
}
