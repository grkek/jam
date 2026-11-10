//! Directory commands transported to a separate EL0 filesystem actor.
//!
//! The console driver supplies the boot-selected service identity. Requests and
//! replies use exact sender, tag, and correlation matching. Listings pull one
//! child per request and render immediately instead of buffering a whole tree.
//! This first slice accepts absolute paths only and rejects unsupported options.

use super::{Terminal, fail};
use foundation::{
    address::LocalAddress,
    console,
    filesystem::{self, Status},
    messaging::RECEIVED_HEADER_BYTE_COUNT,
    system_call_status::SystemCallStatus,
    tag::Tag,
};
use runtime::inbox::{self, MessageSelector};

impl Terminal {
    /// Resolves the filesystem once through the authenticated console connection.
    /// `None` when the driver reports that no filesystem is bound.
    fn bind_filesystem(&mut self) -> Option<LocalAddress> {
        if let Some(address) = self.filesystem_address {
            return Some(address);
        }
        let correlation = self.request(console::FILESYSTEM_ADDRESS_TAG, &[]);
        let mut record = [0; RECEIVED_HEADER_BYTE_COUNT + console::ADDRESS_BYTE_COUNT];
        let payload = self.response(console::FILESYSTEM_BOUND_TAG, correlation, &mut record);
        let address = console::decode_address(payload)?;
        self.filesystem_address = Some(address);
        Some(address)
    }

    /// Sends one operation, retrying admission only; accepted mutations are never replayed.
    fn directory_request<'buffer>(
        &mut self,
        filesystem_address: LocalAddress,
        tag: Tag,
        payload: &[u8],
        record: &'buffer mut [u8],
    ) -> &'buffer [u8] {
        self.correlation = self.correlation.checked_add(1).unwrap_or_else(|| fail());
        let correlation = self.correlation;
        loop {
            match inbox::tell_actor(filesystem_address, tag, correlation, payload) {
                Ok(()) => break,
                Err(error) if error.status == SystemCallStatus::BudgetExhausted => {
                    core::hint::spin_loop();
                }
                Err(_) => fail(),
            }
        }
        inbox::await_matching(
            MessageSelector {
                sender: Some(filesystem_address),
                tag: Some(filesystem::RESPONSE_TAG),
                correlation: Some(correlation),
            },
            record,
        )
        .unwrap_or_else(|_| fail())
        .payload
    }

    /// Explains a failed operation to the user.
    fn report_status(&mut self, status: Status) {
        let message = match status {
            Status::Accepted => return,
            Status::InvalidRequest => {
                "Invalid path: use printable names; no empty or dot segments, no trailing slash."
            }
            Status::NotFound => "Directory or parent does not exist.",
            Status::AlreadyExists => "Directory already exists.",
            Status::CapacityExhausted => "Directory capacity exhausted.",
        };
        self.print(format_args!("{message}\n"));
    }

    /// Executes either spelling with the same argument validation and transport.
    #[inline(never)]
    pub(super) fn directory_command(&mut self, create: bool, arguments: &[&[u8]]) {
        let path = match arguments {
            [] if !create => b"/".as_slice(),
            [path] => *path,
            _ => {
                self.usage(if create {
                    "create-directory <absolute-path>"
                } else {
                    "list-directory [absolute-path]"
                });
                return;
            }
        };
        if !path.starts_with(b"/") || path.len() > filesystem::MAXIMUM_PATH_BYTE_COUNT {
            self.print(format_args!(
                "Use an absolute path of at most {} bytes.\n",
                filesystem::MAXIMUM_PATH_BYTE_COUNT
            ));
            return;
        }
        let Some(filesystem_address) = self.bind_filesystem() else {
            self.print(format_args!("Filesystem is not available.\n"));
            return;
        };

        let mut request = [0; filesystem::MAXIMUM_REQUEST_BYTE_COUNT];
        let mut record = [0; RECEIVED_HEADER_BYTE_COUNT + filesystem::MAXIMUM_RESPONSE_BYTE_COUNT];
        let mut cursor = 0_u16;
        let mut listed_any = false;

        loop {
            let (tag, length) = if create {
                let length =
                    filesystem::encode_create_request(path, &mut request).unwrap_or_else(|| fail());
                (filesystem::CREATE_DIRECTORY_TAG, length)
            } else {
                let length = filesystem::encode_list_request(cursor, path, &mut request)
                    .unwrap_or_else(|| fail());
                (filesystem::LIST_DIRECTORY_TAG, length)
            };
            let payload = request.get(..length).unwrap_or_else(|| fail());
            let bytes = self.directory_request(filesystem_address, tag, payload, &mut record);

            let Some(response) = filesystem::decode_response(bytes) else {
                self.print(format_args!("Invalid filesystem response.\n"));
                return;
            };
            if response.status != Status::Accepted {
                self.report_status(response.status);
                return;
            }
            if create {
                self.write(b"Directory created (memory only).\n");
                return;
            }
            if response.next_cursor == 0 {
                if !listed_any {
                    self.write(b"(empty)\n");
                }
                return;
            }
            if response.next_cursor <= cursor {
                fail();
            }
            self.write(response.name);
            self.write(b"/\n");
            cursor = response.next_cursor;
            listed_any = true;
        }
    }
}
