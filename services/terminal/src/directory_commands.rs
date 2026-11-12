//! Handle-based directory commands for the EL0 terminal.
//!
//! The terminal owns canonical paths. The filesystem sees only individual names
//! within a `WALK` request and remains authoritative for existence and type.
//! A single attached root handle is kept for the session; every other handle is
//! released after its command. Requests match exact sender, tag, and correlation.
//! Successful mutations are never retransmitted.

use super::{
    Terminal,
    directory_path::{DirectoryPath, MAXIMUM_DEPTH, MAXIMUM_PATH_BYTE_COUNT},
    fail,
};
use foundation::{
    address::LocalAddress,
    console,
    file_system::{self, NodeKind, NodeStatus, OpenMode},
    file_system_handle::FileSystemHandle,
    file_system_protocol::{self as protocol, Status},
    messaging::{MAXIMUM_RECORD_BYTE_COUNT, RECEIVED_HEADER_BYTE_COUNT},
    system_call_status::SystemCallStatus,
    tag::Tag,
};
use runtime::inbox::{self, MessageSelector};

/// Handle, component count, component lengths, and canonical path bytes.
const MAXIMUM_WALK_REQUEST_BYTE_COUNT: usize = 8 + 1 + MAXIMUM_DEPTH + MAXIMUM_PATH_BYTE_COUNT;
/// Room for one handle, kind, and maximum valid node name.
const MAXIMUM_CREATE_REQUEST_BYTE_COUNT: usize = 8 + 1 + file_system::MAXIMUM_NAME_BYTE_COUNT;
/// Room for one filesystem response wrapped in a received actor message.
const RESPONSE_RECORD_BYTE_COUNT: usize =
    RECEIVED_HEADER_BYTE_COUNT + protocol::MAXIMUM_RESPONSE_BYTE_COUNT;

const _: () = assert!(MAXIMUM_WALK_REQUEST_BYTE_COUNT <= MAXIMUM_RECORD_BYTE_COUNT);

impl Terminal {
    /// Discovers the boot-authorized filesystem actor through the console driver.
    #[inline(never)]
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

    /// Sends a request once, retrying only a refused message admission.
    ///
    /// The response is authenticated by sender, tag, and correlation. A request
    /// accepted by the kernel is never retried, even when it mutates the filesystem.
    #[inline(never)]
    fn filesystem_request<'record>(
        &mut self,
        filesystem_address: LocalAddress,
        tag: Tag,
        payload: &[u8],
        record: &'record mut [u8],
    ) -> (Status, &'record [u8]) {
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

        let response = inbox::await_matching(
            MessageSelector {
                sender: Some(filesystem_address),
                tag: Some(protocol::RESPONSE_TAG),
                correlation: Some(correlation),
            },
            record,
        )
        .unwrap_or_else(|_| fail());
        protocol::decode_response(response.payload).unwrap_or_else(|| fail())
    }

    /// Lazily attaches a single persistent root handle to this terminal session.
    #[inline(never)]
    fn filesystem_root_handle(
        &mut self,
        filesystem_address: LocalAddress,
    ) -> Result<FileSystemHandle, Status> {
        if let Some(handle) = self.filesystem_root_handle {
            return Ok(handle);
        }

        let mut record = [0; RESPONSE_RECORD_BYTE_COUNT];
        let (status, body) =
            self.filesystem_request(filesystem_address, protocol::ATTACH_TAG, &[], &mut record);
        if status != Status::Accepted {
            return Err(status);
        }
        let handle = protocol::decode_handle_body(body).unwrap_or_else(|| fail());
        self.filesystem_root_handle = Some(handle);
        Ok(handle)
    }

    /// Obtains a temporary handle by walking canonical components from the root.
    ///
    /// A root path uses a zero-component walk, which clones the attached handle.
    #[inline(never)]
    fn walk_path(
        &mut self,
        filesystem_address: LocalAddress,
        path: &DirectoryPath,
    ) -> Result<FileSystemHandle, Status> {
        let root_handle = self.filesystem_root_handle(filesystem_address)?;
        let mut names: [&[u8]; MAXIMUM_DEPTH] = [&[]; MAXIMUM_DEPTH];
        let mut count = 0;
        for name in path
            .as_bytes()
            .split(|byte| *byte == b'/')
            .filter(|name| !name.is_empty())
        {
            *names.get_mut(count).unwrap_or_else(|| fail()) = name;
            count += 1;
        }

        let mut request = [0; MAXIMUM_WALK_REQUEST_BYTE_COUNT];
        let length = protocol::encode_walk_request(root_handle, &names[..count], &mut request)
            .unwrap_or_else(|| fail());
        let mut record = [0; RESPONSE_RECORD_BYTE_COUNT];
        let (status, body) = self.filesystem_request(
            filesystem_address,
            protocol::WALK_TAG,
            request.get(..length).unwrap_or_else(|| fail()),
            &mut record,
        );
        if status != Status::Accepted {
            return Err(status);
        }
        Ok(protocol::decode_handle_body(body).unwrap_or_else(|| fail()))
    }

    /// Sends a request whose payload is exactly a filesystem handle.
    #[inline(never)]
    fn handle_request<'record>(
        &mut self,
        filesystem_address: LocalAddress,
        tag: Tag,
        handle: FileSystemHandle,
        record: &'record mut [u8],
    ) -> (Status, &'record [u8]) {
        let mut request = [0; 8];
        let length =
            protocol::encode_handle_request(handle, &mut request).unwrap_or_else(|| fail());
        self.filesystem_request(
            filesystem_address,
            tag,
            request.get(..length).unwrap_or_else(|| fail()),
            record,
        )
    }

    /// Reads the type and size of a node without opening it.
    #[inline(never)]
    fn statistics_handle(
        &mut self,
        filesystem_address: LocalAddress,
        handle: FileSystemHandle,
    ) -> Result<NodeStatus, Status> {
        let mut record = [0; RESPONSE_RECORD_BYTE_COUNT];
        let (status, body) = self.handle_request(
            filesystem_address,
            protocol::STATISTICS_TAG,
            handle,
            &mut record,
        );
        if status != Status::Accepted {
            return Err(status);
        }
        Ok(protocol::decode_statistics_body(body).unwrap_or_else(|| fail()))
    }

    /// Releases a temporary handle even after the operation using it failed.
    #[inline(never)]
    fn clunk_handle(&mut self, filesystem_address: LocalAddress, handle: FileSystemHandle) {
        let mut record = [0; RESPONSE_RECORD_BYTE_COUNT];
        let (status, body) =
            self.handle_request(filesystem_address, protocol::CLUNK_TAG, handle, &mut record);
        if status != Status::Accepted || !body.is_empty() {
            fail();
        }
    }

    /// Opens a handle for directory listing; opening never transfers ownership.
    #[inline(never)]
    fn open_directory_handle(
        &mut self,
        filesystem_address: LocalAddress,
        handle: FileSystemHandle,
    ) -> Result<(), Status> {
        let mut request = [0; 9];
        let length = protocol::encode_open_request(handle, OpenMode::Read, &mut request)
            .unwrap_or_else(|| fail());
        let mut record = [0; RESPONSE_RECORD_BYTE_COUNT];
        let (status, body) = self.filesystem_request(
            filesystem_address,
            protocol::OPEN_TAG,
            request.get(..length).unwrap_or_else(|| fail()),
            &mut record,
        );
        if status != Status::Accepted {
            return Err(status);
        }
        if !body.is_empty() {
            fail();
        }
        Ok(())
    }

    /// Reports every public filesystem failure without mislabeling handle errors.
    fn report_filesystem_status(&mut self, status: Status) {
        let description = match status {
            Status::Accepted => return,
            Status::InvalidRequest => "Filesystem rejected the request.",
            Status::InvalidHandle => "Filesystem handle is invalid.",
            Status::StaleHandle => "Filesystem handle refers to a removed node.",
            Status::InvalidName => {
                "Invalid name: use printable ASCII, at most 32 bytes, without slashes."
            }
            Status::NotFound => "File or directory not found.",
            Status::AlreadyExists => "File or directory already exists.",
            Status::NotADirectory => "A path component is not a directory.",
            Status::IsADirectory => "Expected a file, found a directory.",
            Status::NotEmpty => "Directory is not empty.",
            Status::AlreadyOpen => "Filesystem handle is already open.",
            Status::NotOpen => "Filesystem handle is not open.",
            Status::PermissionDenied => "Filesystem permission denied.",
            Status::CannotRemoveRoot => "Cannot remove the root directory.",
            Status::TooManyComponents => "Path has too many components.",
            Status::CapacityExhausted => "Filesystem node capacity exhausted.",
            Status::HandlesExhausted => "Filesystem handle capacity exhausted.",
            Status::OutOfSpace => "Filesystem block storage exhausted.",
            Status::FileTooLarge => "File exceeds the filesystem size limit.",
        };
        self.print(format_args!("{description}\n"));
    }

    /// Ensures a cancelled component exists as a directory before applying `..`.
    ///
    /// This prevents `/missing/..` or `/file/..` from silently becoming `/`.
    #[inline(never)]
    fn verify_directory_path(
        &mut self,
        filesystem_address: LocalAddress,
        path: &DirectoryPath,
    ) -> bool {
        let handle = match self.walk_path(filesystem_address, path) {
            Ok(handle) => handle,
            Err(status) => {
                self.report_filesystem_status(status);
                return false;
            }
        };
        let result = self.statistics_handle(filesystem_address, handle);
        self.clunk_handle(filesystem_address, handle);
        match result {
            Ok(statistics) if statistics.kind == NodeKind::Directory => true,
            Ok(_) => {
                self.report_filesystem_status(Status::NotADirectory);
                false
            }
            Err(status) => {
                self.report_filesystem_status(status);
                false
            }
        }
    }

    /// Normalizes a user path, verifying every component discarded by `..`.
    ///
    /// Existence of the final canonical target is checked by the command itself.
    #[inline(never)]
    fn resolve_directory_path(
        &mut self,
        filesystem_address: LocalAddress,
        input: &[u8],
    ) -> Option<DirectoryPath> {
        if input.is_empty() || input.len() > MAXIMUM_PATH_BYTE_COUNT {
            self.print(format_args!(
                "Path must contain 1 to {MAXIMUM_PATH_BYTE_COUNT} bytes.\n"
            ));
            return None;
        }

        let mut path = if input.starts_with(b"/") {
            DirectoryPath::root()
        } else {
            self.working_directory
        };

        for component in input.split(|byte| *byte == b'/') {
            match component {
                b"" | b"." => {}
                b".." => {
                    if !path.is_root() && !self.verify_directory_path(filesystem_address, &path) {
                        return None;
                    }
                    path.parent();
                }
                _ => {
                    if path.append(component).is_none() {
                        self.print(format_args!(
                            "Invalid component or path exceeds {MAXIMUM_PATH_BYTE_COUNT} bytes / {MAXIMUM_DEPTH} levels.\n"
                        ));
                        return None;
                    }
                }
            }
        }
        Some(path)
    }

    /// Changes working directory only after an authoritative type check.
    #[inline(never)]
    pub(super) fn change_directory(&mut self, arguments: &[&[u8]]) {
        let input = match arguments {
            [] => b"/".as_slice(),
            [path] => *path,
            _ => {
                self.usage("change-directory [path]");
                return;
            }
        };
        let Some(filesystem_address) = self.bind_filesystem() else {
            self.write(b"Filesystem is not available.\n");
            return;
        };
        let Some(path) = self.resolve_directory_path(filesystem_address, input) else {
            return;
        };
        if self.verify_directory_path(filesystem_address, &path) {
            self.working_directory = path;
        }
    }

    /// Prints the session's canonical absolute working directory.
    pub(super) fn show_working_directory(&mut self, arguments: &[&[u8]]) {
        if !self.rejects_arguments("working-directory", arguments) {
            let path = self.working_directory;
            self.write(path.as_bytes());
            self.write(b"\n");
        }
    }

    /// Implements `md` and `ld` using filesystem-issued handles.
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
        let final_input_component = options
            .path
            .rsplit(|byte| *byte == b'/')
            .next()
            .unwrap_or(b"");
        if make
            && (options.path.ends_with(b"/")
                || final_input_component == b"."
                || final_input_component == b"..")
        {
            self.write(b"Creation requires a final directory name (not /, . or ..).\n");
            return;
        }
        let Some(filesystem_address) = self.bind_filesystem() else {
            self.write(b"Filesystem is not available.\n");
            return;
        };
        let Some(path) = self.resolve_directory_path(filesystem_address, options.path) else {
            return;
        };
        if make {
            self.create_directory(filesystem_address, &path);
        } else if options.directory_itself {
            self.describe_target(filesystem_address, &path, options.long);
        } else {
            self.list_directory(filesystem_address, &path, options);
        }
    }

    /// Creates one child under its existing parent, releasing both returned handles.
    #[inline(never)]
    fn create_directory(&mut self, filesystem_address: LocalAddress, path: &DirectoryPath) {
        let Some(name) = path.name() else {
            self.write(b"The root directory already exists.\n");
            return;
        };
        let mut parent = *path;
        parent.parent();
        let parent_handle = match self.walk_path(filesystem_address, &parent) {
            Ok(handle) => handle,
            Err(status) => {
                self.report_filesystem_status(status);
                return;
            }
        };

        let mut request = [0; MAXIMUM_CREATE_REQUEST_BYTE_COUNT];
        let request_length =
            protocol::encode_create_request(parent_handle, NodeKind::Directory, name, &mut request)
                .unwrap_or_else(|| fail());
        let mut record = [0; RESPONSE_RECORD_BYTE_COUNT];
        let (status, body) = self.filesystem_request(
            filesystem_address,
            protocol::CREATE_TAG,
            request.get(..request_length).unwrap_or_else(|| fail()),
            &mut record,
        );
        let created_handle = if status == Status::Accepted {
            Some(protocol::decode_handle_body(body).unwrap_or_else(|| fail()))
        } else {
            None
        };
        if let Some(created_handle) = created_handle {
            self.clunk_handle(filesystem_address, created_handle);
        }
        self.clunk_handle(filesystem_address, parent_handle);

        if status == Status::Accepted {
            self.write(b"Directory created (memory only).\n");
        } else {
            self.report_filesystem_status(status);
        }
    }

    /// Implements `ld -d` by inspecting the requested node, not its children.
    #[inline(never)]
    fn describe_target(
        &mut self,
        filesystem_address: LocalAddress,
        path: &DirectoryPath,
        long: bool,
    ) {
        let handle = match self.walk_path(filesystem_address, path) {
            Ok(handle) => handle,
            Err(status) => {
                self.report_filesystem_status(status);
                return;
            }
        };
        let result = self.statistics_handle(filesystem_address, handle);
        self.clunk_handle(filesystem_address, handle);
        match result {
            Ok(statistics) => {
                if long {
                    self.write(b"TYPE  NAME\n");
                }
                let kind = if long {
                    if statistics.kind == NodeKind::Directory {
                        "dir   "
                    } else {
                        "file  "
                    }
                } else {
                    ""
                };
                let suffix = if statistics.kind == NodeKind::Directory && !path.is_root() {
                    "/"
                } else {
                    ""
                };
                let name = core::str::from_utf8(path.as_bytes()).unwrap_or_else(|_| fail());
                self.print(format_args!("{kind}{name}{suffix}\n"));
            }
            Err(status) => self.report_filesystem_status(status),
        }
    }

    /// Opens a directory and streams its entries without retaining a whole listing.
    #[inline(never)]
    fn list_directory(
        &mut self,
        filesystem_address: LocalAddress,
        path: &DirectoryPath,
        options: DirectoryOptions<'_>,
    ) {
        let handle = match self.walk_path(filesystem_address, path) {
            Ok(handle) => handle,
            Err(status) => {
                self.report_filesystem_status(status);
                return;
            }
        };
        if let Err(status) = self.open_directory_handle(filesystem_address, handle) {
            self.clunk_handle(filesystem_address, handle);
            self.report_filesystem_status(status);
            return;
        }
        let listing_result = self.stream_directory_entries(filesystem_address, handle, options);
        self.clunk_handle(filesystem_address, handle);
        if let Err(status) = listing_result {
            self.report_filesystem_status(status);
        }
    }

    /// Walks the cursor one entry per request, printing as replies arrive.
    #[inline(never)]
    fn stream_directory_entries(
        &mut self,
        filesystem_address: LocalAddress,
        handle: FileSystemHandle,
        options: DirectoryOptions<'_>,
    ) -> Result<(), Status> {
        let mut cursor = 0_u16;
        let mut listed_any = false;
        loop {
            let mut request = [0; 10];
            let request_length =
                protocol::encode_read_directory_request(handle, cursor, &mut request)
                    .unwrap_or_else(|| fail());
            let mut record = [0; RESPONSE_RECORD_BYTE_COUNT];
            let (status, body) = self.filesystem_request(
                filesystem_address,
                protocol::READ_DIRECTORY_TAG,
                request.get(..request_length).unwrap_or_else(|| fail()),
                &mut record,
            );
            if status != Status::Accepted {
                return Err(status);
            }
            let entry = protocol::decode_directory_body(body).unwrap_or_else(|| fail());
            if entry.is_end() {
                if !entry.name.is_empty() {
                    fail();
                }
                break;
            }
            if entry.next_cursor <= cursor || !file_system::is_valid_name(entry.name) {
                fail();
            }
            cursor = entry.next_cursor;
            if !options.all && entry.name.starts_with(b".") {
                continue;
            }
            if options.long && !listed_any {
                self.write(b"TYPE  NAME\n");
            }
            let kind = if options.long {
                if entry.kind == NodeKind::Directory {
                    "dir   "
                } else {
                    "file  "
                }
            } else {
                ""
            };
            let suffix = if entry.kind == NodeKind::Directory {
                "/"
            } else {
                ""
            };
            let name = core::str::from_utf8(entry.name).unwrap_or_else(|_| fail());
            self.print(format_args!("{kind}{name}{suffix}\n"));
            listed_any = true;
        }
        if !listed_any {
            self.write(b"(no matching entries)\n");
        }
        Ok(())
    }
}

/// Options parsed for `ld` or `md` before any filesystem mutation.
#[derive(Clone, Copy)]
struct DirectoryOptions<'a> {
    /// Input path to resolve against the terminal working directory.
    path: &'a [u8],
    /// Include entries whose names start with `.`.
    all: bool,
    /// Show the type of each entry.
    long: bool,
    /// As in `ls -d`, show the target itself rather than listing its children.
    directory_itself: bool,
}

impl<'a> DirectoryOptions<'a> {
    /// Accepts clustered short options and `--` before a single path argument.
    fn parse(make: bool, arguments: &[&'a [u8]]) -> Option<Self> {
        let mut path = None;
        let mut all = false;
        let mut long = false;
        let mut directory_itself = false;
        let mut options_enabled = true;
        for &argument in arguments {
            if options_enabled && argument == b"--" {
                options_enabled = false;
                continue;
            }
            if !make && options_enabled && argument.starts_with(b"--") {
                match argument {
                    b"--all" => all = true,
                    b"--long" => long = true,
                    b"--directories" => directory_itself = true,
                    _ => return None,
                }
            } else if !make && options_enabled && argument.starts_with(b"-") {
                let flags = argument.get(1..)?;
                if flags.is_empty() {
                    return None;
                }
                for flag in flags {
                    match flag {
                        b'a' => all = true,
                        b'l' => long = true,
                        b'd' => directory_itself = true,
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
            directory_itself,
        })
    }
}
