//! Build, image, run, test, and check automation for JAM.
//!
//! This tool builds Genesis before the kernel, supplies its ELF path, collects
//! board images, manages firmware checkouts, and launches QEMU. Cargo build
//! scripts only configure compilation; they never invoke this tool or Cargo.

use std::{
    env,
    ffi::OsString,
    fs,
    path::{Path, PathBuf},
    process::{Command, ExitCode},
};

/// Workspace packages checked for the bare-metal architecture.
const BARE_METAL_PACKAGES: [&str; 9] = [
    "kernel",
    "runtime",
    "genesis",
    "terminal",
    "registry",
    "observatory",
    "distribution",
    "pl011_console",
    "virtio_network",
];

/// Rust target the kernel and every actor are built for.
const TARGET_TRIPLE: &str = "aarch64-unknown-none-softfloat";
/// Cargo feature of the kernel crate that selects the Raspberry Pi 4 platform.
const RASPBERRY_PI_4_FEATURE: &str = "raspberry-pi-4";
/// Cargo feature of the kernel crate that compiles in the boot-time stress plan.
const STRESS_FEATURE: &str = "stress";
/// A `kernel8.img` larger than this means the linker script's load addresses are wrong.
const MAXIMUM_IMAGE_BYTE_COUNT: u64 = 16 * 1024 * 1024;
/// Repository containing Raspberry Pi boot firmware and device trees.
const FIRMWARE_REPOSITORY: &str = "https://github.com/raspberrypi/firmware";

/// Firmware files the Raspberry Pi 4 needs on its SD card, relative to the firmware directory.
const FIRMWARE_FILES: [&str; 4] = [
    "start4.elf",
    "fixup4.dat",
    "bcm2711-rpi-4-b.dtb",
    "overlays/disable-bt.dtbo",
];
/// Device tree the QEMU `raspi4b` machine is given.
const RASPBERRY_PI_4_DEVICE_TREE: &str = "bcm2711-rpi-4-b.dtb";

/// Firmware configuration. The kernel depends on every line, so all are written out.
const RASPBERRY_PI_4_CONFIG: &str = "\
arm_64bit=1
kernel=kernel8.img
arm_peri_high=0
enable_gic=1
enable_uart=1
init_uart_clock=48000000
dtoverlay=disable-bt
";

/// Arguments for `qemu-system-aarch64` on the `virt` machine, before the kernel path.
const VIRTUAL_ARGUMENTS: [&str; 9] = [
    "-machine",
    "virt,gic-version=2",
    "-cpu",
    "cortex-a72",
    "-smp",
    "8",
    "-m",
    "512M",
    "-nographic",
];

/// A board JAM can be built for.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Board {
    /// QEMU's AArch64 `virt` machine.
    Virtual,
    /// Raspberry Pi 4 Model B.
    RaspberryPi4,
}

impl Board {
    /// Every supported board, in build order.
    const ALL: [Board; 2] = [Board::Virtual, Board::RaspberryPi4];

    /// Name used on the command line and for the output directory.
    fn name(self) -> &'static str {
        match self {
            Board::Virtual => "virtual",
            Board::RaspberryPi4 => "raspberry-pi-4",
        }
    }

    /// Looks a board up by its command-line name.
    fn from_name(name: &str) -> Option<Board> {
        Board::ALL.into_iter().find(|board| board.name() == name)
    }

    /// Kernel Cargo features this board needs.
    fn features(self) -> &'static [&'static str] {
        match self {
            Board::Virtual => &[],
            Board::RaspberryPi4 => &[RASPBERRY_PI_4_FEATURE],
        }
    }
}

/// Options shared by the build and run commands.
struct Options {
    /// Build with the release profile instead of the dev profile.
    release: bool,
    /// Build only this board; `None` means every board.
    board: Option<Board>,
    /// Compile the boot-time stress plan into the kernel.
    stress: bool,
}

impl Options {
    /// Name of the profile directory under `target/<triple>/`.
    fn profile_directory(&self) -> &'static str {
        if self.release { "release" } else { "debug" }
    }

    /// Kernel Cargo features for `board`, plus the stress plan when requested.
    fn kernel_features(&self, board: Board) -> Vec<&'static str> {
        let mut features = board.features().to_vec();
        if self.stress {
            features.push(STRESS_FEATURE);
        }
        features
    }
}

/// Parses `--release`, `--stress` and `--board <name>`.
fn parse_options(arguments: &[String]) -> Result<Options, String> {
    let mut options = Options {
        release: false,
        board: None,
        stress: false,
    };
    let mut remaining = arguments.iter();
    while let Some(argument) = remaining.next() {
        match argument.as_str() {
            "--release" => options.release = true,
            "--stress" => options.stress = true,
            "--board" => {
                let name = remaining.next().ok_or("--board needs a board name")?;
                options.board =
                    Some(Board::from_name(name).ok_or_else(|| format!("unknown board `{name}`"))?);
            }
            other => return Err(format!("unknown option `{other}`")),
        }
    }
    Ok(options)
}

/// Repository root, two levels above `tools/xtask`.
fn root_directory() -> Result<PathBuf, String> {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .map_err(|error| format!("cannot locate the workspace root: {error}"))
}

/// Cargo executable supplied by Cargo itself, or the executable on `PATH`.
fn cargo_program() -> OsString {
    env::var_os("CARGO").unwrap_or_else(|| OsString::from("cargo"))
}

/// Path of the Genesis ELF produced for the selected build profile.
fn genesis_program_path(root: &Path, options: &Options) -> PathBuf {
    root.join("target")
        .join(TARGET_TRIPLE)
        .join(options.profile_directory())
        .join("genesis")
}

/// Runs `command` and turns a failure to start or a nonzero exit into an error.
fn run_checked(command: &mut Command, description: &str) -> Result<(), String> {
    let status = command
        .status()
        .map_err(|error| format!("cannot start {description}: {error}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("{description} failed with {status}"))
    }
}

/// A `cargo build` for `package` in the repository root, with the options applied.
fn cargo_build(root: &Path, package: &str, options: &Options, features: &[&str]) -> Command {
    let mut command = Command::new(cargo_program());
    command
        .current_dir(root)
        .args(["build", "--package", package, "--target", TARGET_TRIPLE]);
    // Keep Cargo output consistent with the paths used for image collection.
    command.arg("--target-dir").arg(root.join("target"));
    if package == "kernel" {
        command.env("GENESIS_ELF_PATH", genesis_program_path(root, options));
        command.env(
            "TERMINAL_ELF_PATH",
            genesis_program_path(root, options).with_file_name("terminal"),
        );
    }
    if options.release {
        command.arg("--release");
    }
    if !features.is_empty() {
        command.args(["--features", &features.join(",")]);
    }
    command
}

/// Finds an `objcopy`: on `PATH` first, then inside the rustup toolchains.
fn find_object_copy() -> Result<PathBuf, String> {
    for name in ["rust-objcopy", "llvm-objcopy"] {
        if Command::new(name)
            .arg("--version")
            .output()
            .is_ok_and(|output| output.status.success())
        {
            return Ok(PathBuf::from(name));
        }
    }
    if let Some(home) = env::var_os("HOME") {
        let toolchains = Path::new(&home).join(".rustup/toolchains");
        for toolchain in fs::read_dir(toolchains).into_iter().flatten().flatten() {
            let rustlib = toolchain.path().join("lib/rustlib");
            for host in fs::read_dir(rustlib).into_iter().flatten().flatten() {
                let candidate = host.path().join("bin/llvm-objcopy");
                if candidate.is_file() {
                    return Ok(candidate);
                }
            }
        }
    }
    Err(
        "no objcopy found: run `rustup component add llvm-tools` and \
         `cargo install cargo-binutils`"
            .to_string(),
    )
}

/// Checks that a firmware checkout contains every required boot file.
fn validate_firmware(directory: &Path) -> Result<(), String> {
    for filename in FIRMWARE_FILES {
        let source = directory.join(filename);
        if !source.is_file() {
            return Err(format!(
                "required firmware file is missing: {}",
                source.display()
            ));
        }
    }
    Ok(())
}

/// Returns the board's firmware boot directory, cloning it on first use.
///
/// The checkout lives under `target/<board>/firmware`. Existing checkouts
/// are validated and reused without fetching updates. New checkouts are
/// staged separately so a failed download cannot become the cached firmware.
fn firmware_directory(output: &Path) -> Result<PathBuf, String> {
    let checkout_directory = output.join("firmware");
    let boot_directory = checkout_directory.join("boot");

    if checkout_directory.exists() {
        validate_firmware(&boot_directory)?;
        return Ok(boot_directory);
    }

    let staging_directory = output.join(format!("firmware-download-{}", std::process::id()));

    fs::create_dir(&staging_directory).map_err(|error| {
        format!(
            "cannot create firmware staging directory {}: {error}",
            staging_directory.display()
        )
    })?;

    println!(
        "xtask: cloning firmware into {}",
        checkout_directory.display()
    );

    let result = run_checked(
        Command::new("git")
            .args(["clone", "--depth", "1", "--single-branch"])
            .arg(FIRMWARE_REPOSITORY)
            .arg(&staging_directory),
        "cloning Raspberry Pi firmware",
    )
    .and_then(|()| validate_firmware(&staging_directory.join("boot")))
    .and_then(|()| {
        fs::rename(&staging_directory, &checkout_directory).map_err(|error| {
            format!(
                "cannot move firmware into {}: {error}",
                checkout_directory.display()
            )
        })
    });

    if let Err(error) = result {
        if staging_directory.exists() {
            fs::remove_dir_all(&staging_directory).map_err(|cleanup_error| {
                format!(
                    "{error}; cannot remove {}: {cleanup_error}",
                    staging_directory.display()
                )
            })?;
        }
        return Err(error);
    }

    Ok(boot_directory)
}

/// Copies the required firmware files beside the board's kernel image.
///
/// Firmware is downloaded on first use and reused on subsequent builds.
/// Missing firmware or a failed download makes the build fail.
fn copy_firmware(output: &Path) -> Result<(), String> {
    let directory = firmware_directory(output)?;

    for filename in FIRMWARE_FILES {
        let source = directory.join(filename);
        let destination = output.join(filename);

        if let Some(parent_directory) = destination.parent() {
            fs::create_dir_all(parent_directory)
                .map_err(|error| format!("{}: {error}", parent_directory.display()))?;
        }

        fs::copy(&source, &destination).map_err(|error| {
            format!(
                "cannot copy {} to {}: {error}",
                source.display(),
                destination.display()
            )
        })?;
    }

    println!("copied firmware files from {}", directory.display());
    Ok(())
}

/// Turns the kernel ELF into `kernel8.img` and writes the files the SD card needs.
fn create_raspberry_pi_4_image(kernel_elf: &Path, output: &Path) -> Result<(), String> {
    let image = output.join("kernel8.img");
    run_checked(
        Command::new(find_object_copy()?)
            .arg("--output-target=binary")
            .arg(kernel_elf)
            .arg(&image),
        "objcopy",
    )?;

    let byte_count = fs::metadata(&image)
        .map_err(|error| format!("{}: {error}", image.display()))?
        .len();
    if byte_count > MAXIMUM_IMAGE_BYTE_COUNT {
        return Err(format!(
            "kernel8.img is {byte_count} bytes; check the load addresses in the linker script"
        ));
    }

    fs::write(output.join("config.txt"), RASPBERRY_PI_4_CONFIG)
        .map_err(|error| format!("config.txt: {error}"))?;
    copy_firmware(output)?;
    println!("created {} ({byte_count} bytes)", image.display());
    Ok(())
}

/// Builds Genesis and the kernel for `board`, and collects the outputs in `target/<board>/`.
fn build_board(root: &Path, board: Board, options: &Options) -> Result<PathBuf, String> {
    println!("== building for {}", board.name());
    build_genesis(root, options)?;
    run_checked(
        &mut cargo_build(root, "terminal", options, &[]),
        "building terminal",
    )?;
    run_checked(
        &mut cargo_build(root, "kernel", options, &options.kernel_features(board)),
        "building the kernel",
    )?;

    let built = root
        .join("target")
        .join(TARGET_TRIPLE)
        .join(options.profile_directory())
        .join("kernel");
    let output = root.join("target").join(board.name());
    fs::create_dir_all(&output).map_err(|error| format!("{}: {error}", output.display()))?;
    let kernel_elf = output.join("kernel.elf");
    fs::copy(&built, &kernel_elf)
        .map_err(|error| format!("cannot copy {}: {error}", built.display()))?;

    if board == Board::RaspberryPi4 {
        create_raspberry_pi_4_image(&kernel_elf, &output)?;
    }
    Ok(output)
}

/// Builds the selected board, or every board.
fn build(root: &Path, options: &Options) -> Result<(), String> {
    let boards: Vec<Board> = options
        .board
        .map_or(Board::ALL.to_vec(), |board| vec![board]);
    for board in boards {
        let output = build_board(root, board, options)?;
        println!("   output: {}", output.display());
    }
    Ok(())
}

/// Boots the kernel on QEMU `virt`.
fn run_virtual(root: &Path, options: &Options) -> Result<(), String> {
    let output = build_board(root, Board::Virtual, options)?;
    println!("xtask: booting on QEMU (press Ctrl-A then X to quit)");
    run_checked(
        Command::new("qemu-system-aarch64")
            .args(VIRTUAL_ARGUMENTS)
            .arg("-kernel")
            .arg(output.join("kernel.elf")),
        "QEMU",
    )
}

/// Boots the Raspberry Pi 4 image on QEMU `raspi4b`.
fn run_raspberry_pi_4(root: &Path, options: &Options) -> Result<(), String> {
    let output = build_board(root, Board::RaspberryPi4, options)?;
    let device_tree = output.join(RASPBERRY_PI_4_DEVICE_TREE);
    if !device_tree.is_file() {
        return Err(format!(
            "{} is missing after the firmware copy",
            device_tree.display()
        ));
    }
    println!("xtask: booting on QEMU raspi4b (press Ctrl-A then X to quit)");
    run_checked(
        Command::new("qemu-system-aarch64")
            .args(["-M", "raspi4b", "-display", "none"])
            .arg("-kernel")
            .arg(output.join("kernel8.img"))
            .arg("-dtb")
            .arg(&device_tree)
            .args(["-serial", "mon:stdio", "-serial", "null"]),
        "QEMU",
    )
}

/// Builds Genesis and verifies that the ELF needed by the kernel exists.
///
/// Genesis must match the kernel: the kernel's stress plan sends bootstrap messages
/// that only the stress build of Genesis consumes.
fn build_genesis(root: &Path, options: &Options) -> Result<(), String> {
    let features: &[&str] = if options.stress {
        &[STRESS_FEATURE]
    } else {
        &[]
    };

    run_checked(
        &mut cargo_build(root, "genesis", options, features),
        "building Genesis",
    )?;

    let genesis_program = genesis_program_path(root, options);
    if !genesis_program.is_file() {
        return Err(format!(
            "Genesis build did not produce {}",
            genesis_program.display()
        ));
    }

    Ok(())
}

/// Runs the workspace's default host-side tests.
fn test(root: &Path, options: &Options) -> Result<(), String> {
    if options.board.is_some() {
        return Err("host tests do not accept --board".to_string());
    }
    if options.stress {
        return Err("host tests do not accept --stress".to_string());
    }
    let mut command = Command::new(cargo_program());
    command.current_dir(root).arg("test");
    if options.release {
        command.arg("--release");
    }
    run_checked(&mut command, "host tests")
}

/// Checks formatting and Clippy for the host, xtask, and selected boards.
///
/// Genesis is built before checking packages that embed it. Firmware and
/// board images are unnecessary for these checks and are not downloaded.
/// Every board is linted twice, with and without the stress plan, so neither
/// configuration can rot; `--stress` is therefore not needed here.
fn check(root: &Path, options: &Options) -> Result<(), String> {
    run_checked(
        Command::new(cargo_program())
            .current_dir(root)
            .args(["fmt", "--all", "--", "--check"]),
        "workspace formatting",
    )?;
    let manifest = root.join("tools/xtask/Cargo.toml");
    run_checked(
        Command::new(cargo_program())
            .current_dir(root)
            .args(["fmt", "--manifest-path"])
            .arg(&manifest)
            .args(["--", "--check"]),
        "xtask formatting",
    )?;
    run_checked(
        Command::new(cargo_program())
            .current_dir(root)
            .args(["clippy", "--", "-D", "warnings"]),
        "host Clippy",
    )?;
    run_checked(
        Command::new(cargo_program())
            .current_dir(root)
            .args(["clippy", "--manifest-path"])
            .arg(&manifest)
            .args(["--", "-D", "warnings"]),
        "xtask Clippy",
    )?;

    build_genesis(root, options)?;
    run_checked(
        &mut cargo_build(root, "terminal", options, &[]),
        "building terminal",
    )?;
    let boards = options
        .board
        .map_or(Board::ALL.to_vec(), |board| vec![board]);
    for board in boards {
        for stress in [false, true] {
            let mut command = Command::new(cargo_program());
            command
                .current_dir(root)
                .args(["clippy", "--target", TARGET_TRIPLE, "--target-dir"])
                .arg(root.join("target"))
                .env("GENESIS_ELF_PATH", genesis_program_path(root, options))
                .env(
                    "TERMINAL_ELF_PATH",
                    genesis_program_path(root, options).with_file_name("terminal"),
                );
            for package in BARE_METAL_PACKAGES {
                command.args(["--package", package]);
            }
            if options.release {
                command.arg("--release");
            }
            let features: Vec<String> = Options {
                release: options.release,
                board: options.board,
                stress,
            }
            .kernel_features(board)
            .into_iter()
            .map(|feature| format!("kernel/{feature}"))
            .collect();
            if !features.is_empty() {
                command.args(["--features", &features.join(",")]);
            }
            command.args(["--", "-D", "warnings"]);
            let description = format!(
                "Clippy for {}{}",
                board.name(),
                if stress { " with the stress plan" } else { "" }
            );
            run_checked(&mut command, &description)?;
        }
    }
    Ok(())
}

/// Runs the requested command with parsed options.
fn execute() -> Result<(), String> {
    let arguments: Vec<String> = env::args().skip(1).collect();
    let Some((command, remaining)) = arguments.split_first() else {
        print_usage();
        return Ok(());
    };
    let options = parse_options(remaining)?;
    let root = root_directory()?;
    match command.as_str() {
        "build" => build(&root, &options),
        "run" => match options.board.unwrap_or(Board::Virtual) {
            Board::Virtual => run_virtual(&root, &options),
            Board::RaspberryPi4 => run_raspberry_pi_4(&root, &options),
        },
        "run-raspberry-pi-4" => {
            if options.board == Some(Board::Virtual) {
                return Err("run-raspberry-pi-4 cannot use --board virtual".to_string());
            }
            run_raspberry_pi_4(&root, &options)
        }
        "test" => test(&root, &options),
        "check" => check(&root, &options),
        other => Err(format!("unknown command `{other}`")),
    }
}

/// Prints the command list.
fn print_usage() {
    println!(
        "usage: cargo xtask <command> [--release] [--stress] [--board virtual|raspberry-pi-4]\n\n\
         commands:\n  \
         build                 build every board (or one with --board)\n  \
         run                   build and boot the selected board (default: virtual)\n  \
         run-raspberry-pi-4    build and boot QEMU Raspberry Pi 4 board\n  \
         test                  run host tests (--release supported)\n  \
         check                 check formatting and Clippy (with and without --stress)\n\n\
         options:\n  \
         --stress              compile the boot-time stress plan into the kernel\n  \
         --release             use the release profile\n  \
         --board <name>        build or run only this board"
    );
}

/// Reports command errors and returns the corresponding process status.
fn main() -> ExitCode {
    match execute() {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("xtask: {message}");
            print_usage();
            ExitCode::FAILURE
        }
    }
}
