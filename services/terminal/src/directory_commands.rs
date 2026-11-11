//! Directory commands transported to a separate EL0 filesystem actor.
//!
//! The console driver supplies the boot-selected service identity. Requests and
//! replies use exact sender, tag, and correlation matching. Listings pull one
//! child per request and render immediately instead of buffering a whole tree.
//! Paths resolve against terminal-local working state; listings support hidden
//! entries and type display without changing the filesystem wire protocol.

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

    /// Checks existence through a bounded listing request without printing a child.
    fn directory_exists(&mut self, address: LocalAddress, path: &[u8]) -> bool {
        let mut request = [0; filesystem::MAXIMUM_REQUEST_BYTE_COUNT];
        let mut record = [0; RECEIVED_HEADER_BYTE_COUNT + filesystem::MAXIMUM_RESPONSE_BYTE_COUNT];
        let length =
            filesystem::encode_list_request(0, path, &mut request).unwrap_or_else(|| fail());
        let bytes = self.directory_request(
            address,
            filesystem::LIST_DIRECTORY_TAG,
            request.get(..length).unwrap_or_else(|| fail()),
            &mut record,
        );
        let Some(response) = filesystem::decode_response(bytes) else {
            self.write(b"Invalid filesystem response.\n");
            return false;
        };
        self.report_status(response.status);
        response.status == Status::Accepted
    }

    /// Resolves components in order, checking each intermediate directory.
    /// A missing component followed by `..` is an error, not silently erased.
    fn resolve_directory_path(
        &mut self,
        address: LocalAddress,
        input: &[u8],
    ) -> Option<super::directory_path::DirectoryPath> {
        if input.is_empty() || input.len() > filesystem::MAXIMUM_PATH_BYTE_COUNT {
            self.write(b"Path is empty or exceeds 96 bytes.\n");
            return None;
        }
        let mut path = if input.starts_with(b"/") {
            super::directory_path::DirectoryPath::root()
        } else {
            self.working_directory
        };
        let mut components = input.split(|byte| *byte == b'/').peekable();
        while let Some(component) = components.next() {
            match component {
                b"" | b"." => {}
                b".." => path.parent(),
                _ => {
                    if path.append(component).is_none() {
                        self.write(b"Invalid component or resolved path exceeds 96 bytes.\n");
                        return None;
                    }
                    if components.peek().is_some()
                        && !self.directory_exists(address, path.as_bytes())
                    {
                        return None;
                    }
                }
            }
        }
        Some(path)
    }

    /// Changes session state only after the service confirms the destination exists.
    pub(super) fn change_directory(&mut self, arguments: &[&[u8]]) {
        let input = match arguments {
            [] => b"/".as_slice(),
            [path] => *path,
            _ => {
                self.usage("change-directory [path]");
                return;
            }
        };
        let Some(address) = self.bind_filesystem() else {
            self.write(b"Filesystem is not available.\n");
            return;
        };
        let Some(path) = self.resolve_directory_path(address, input) else {
            return;
        };
        if self.directory_exists(address, path.as_bytes()) {
            self.working_directory = path;
        }
    }

    /// Prints this terminal session's canonical working directory.
    pub(super) fn show_working_directory(&mut self, arguments: &[&[u8]]) {
        if !self.rejects_arguments("working-directory", arguments) {
            let path = self.working_directory;
            self.write(path.as_bytes());
            self.write(b"\n");
        }
    }

    /// Executes creation or a bounded listing using the shared path resolver.
    #[inline(never)]
    pub(super) fn directory_command(&mut self, make: bool, arguments: &[&[u8]]) {
        let Some(options) = DirectoryOptions::parse(make, arguments) else {
            self.usage(if make {
                "make-directory <path>"
            } else {
                "list-directory [-lad] [--long] [--all] [--directories] [--] [path]"
            });
            return;
        };
        let Some(address) = self.bind_filesystem() else {
            self.write(b"Filesystem is not available.\n");
            return;
        };
        let Some(path) = self.resolve_directory_path(address, options.path) else {
            return;
        };
        let mut request = [0; filesystem::MAXIMUM_REQUEST_BYTE_COUNT];
        let mut record = [0; RECEIVED_HEADER_BYTE_COUNT + filesystem::MAXIMUM_RESPONSE_BYTE_COUNT];
        let mut cursor = 0_u16;
        let mut listed_any = false;
        loop {
            let (tag, length) = if make {
                (
                    filesystem::CREATE_DIRECTORY_TAG,
                    filesystem::encode_make_request(path.as_bytes(), &mut request),
                )
            } else {
                (
                    filesystem::LIST_DIRECTORY_TAG,
                    filesystem::encode_list_request(cursor, path.as_bytes(), &mut request),
                )
            };
            let length = length.unwrap_or_else(|| fail());
            let bytes = self.directory_request(
                address,
                tag,
                request.get(..length).unwrap_or_else(|| fail()),
                &mut record,
            );
            let Some(response) = filesystem::decode_response(bytes) else {
                self.write(b"Invalid filesystem response.\n");
                return;
            };
            if response.status != Status::Accepted {
                self.report_status(response.status);
                return;
            }
            if make {
                self.write(b"Directory created (memory only).\n");
                return;
            }
            if response.next_cursor == 0 {
                if !listed_any {
                    self.write(b"(no matching entries)\n");
                }
                return;
            }
            if response.next_cursor <= cursor {
                fail();
            }
            cursor = response.next_cursor;
            if !options.all && response.name.starts_with(b".") {
                continue;
            }
            if options.long {
                if !listed_any {
                    self.write(b"TYPE  NAME\n");
                }
                self.write(b"dir   ");
            }
            self.write(response.name);
            self.write(b"/\n");
            listed_any = true;
        }
    }
}

/// Validated options; the current service stores directories only.
struct DirectoryOptions<'a> {
    /// Input path before resolution against the terminal's working directory.
    path: &'a [u8],
    /// Include names beginning with a dot.
    all: bool,
    /// Display the supported entry type alongside its name.
    long: bool,
}

impl<'a> DirectoryOptions<'a> {
    /// Accepts clustered short flags, complete long names, and `--`.
    fn parse(make: bool, arguments: &[&'a [u8]]) -> Option<Self> {
        let mut path = None;
        let mut all = false;
        let mut long = false;
        let mut options = !make;
        for &argument in arguments {
            if options && argument == b"--" {
                options = false;
                continue;
            }
            if options && argument.starts_with(b"--") {
                match argument {
                    b"--all" => all = true,
                    b"--long" => long = true,
                    b"--directories" => {}
                    _ => return None,
                }
            } else if options && argument.starts_with(b"-") {
                let flags = argument.get(1..)?;
                if flags.is_empty() {
                    return None;
                }
                for flag in flags {
                    match flag {
                        b'a' => all = true,
                        b'l' => long = true,
                        b'd' => {}
                        _ => return None,
                    }
                }
            } else if path.replace(argument).is_some() {
                return None;
            }
        }
        if make && path.is_none() {
            return None;
        }
        Some(Self {
            path: path.unwrap_or(b"."),
            all,
            long,
        })
    }
}
