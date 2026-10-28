//! Build, run, and test automation for the workspace.

use std::{
    ffi::OsString,
    path::{Path, PathBuf},
    process::{Command, ExitCode},
};

const PROGRAM_NAME: &str = "xtask";
const AVAILABLE_COMMANDS: [&str; 4] = ["build", "run", "test", "check"];

const KERNEL_PACKAGE_NAME: &str = "kernel";
const KERNEL_TARGET: &str = "aarch64-unknown-none-softfloat";
const BARE_METAL_PACKAGES: [&str; 8] = [
    "kernel",
    "runtime",
    "genesis",
    "registry",
    "observatory",
    "distribution",
    "pl011_console",
    "virtio_network",
];

const QEMU_PROGRAM: &str = "qemu-system-aarch64";
const QEMU_CORE_COUNT: &str = "8";
const QEMU_MEMORY: &str = "512M";

fn main() -> ExitCode {
    let requested_command = std::env::args().nth(1);
    let outcome = match requested_command.as_deref() {
        Some("build") => build_kernel().map(|_| ()),
        Some("run") => build_kernel().and_then(|kernel_path| run_on_qemu(&kernel_path)),
        Some("test") => test(),
        Some("check") => check(),
        _ => Err(format!(
            "usage: cargo {PROGRAM_NAME} <{}>",
            AVAILABLE_COMMANDS.join("|")
        )),
    };

    match outcome {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("{PROGRAM_NAME}: {message}");
            ExitCode::FAILURE
        }
    }
}

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn cargo_program() -> OsString {
    std::env::var_os("CARGO").unwrap_or_else(|| OsString::from("cargo"))
}

fn run_command(program: &OsString, arguments: &[&str]) -> Result<(), String> {
    let status = Command::new(program)
        .args(arguments)
        .current_dir(workspace_root())
        .status()
        .map_err(|error| format!("could not start {}: {error}", program.to_string_lossy()))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!(
            "{} {arguments:?} failed",
            program.to_string_lossy()
        ))
    }
}

fn build_kernel() -> Result<PathBuf, String> {
    // Build the root genesis service binary first so it is available for the kernel embed.
    run_command(
        &cargo_program(),
        &["build", "--package", "genesis", "--target", KERNEL_TARGET],
    )?;

    // Build the kernel package targeting the same bare-metal architecture.
    run_command(
        &cargo_program(),
        &[
            "build",
            "--package",
            KERNEL_PACKAGE_NAME,
            "--target",
            KERNEL_TARGET,
        ],
    )?;

    Ok(workspace_root()
        .join("target")
        .join(KERNEL_TARGET)
        .join("debug")
        .join(KERNEL_PACKAGE_NAME))
}

fn run_on_qemu(kernel_path: &Path) -> Result<(), String> {
    let kernel_path = kernel_path.to_string_lossy();
    eprintln!("{PROGRAM_NAME}: booting on QEMU (press Ctrl-A then X to quit)");
    run_command(
        &OsString::from(QEMU_PROGRAM),
        &[
            "-machine",
            "virt",
            "-cpu",
            "cortex-a72",
            "-smp",
            QEMU_CORE_COUNT,
            "-m",
            QEMU_MEMORY,
            "-nographic",
            "-kernel",
            &kernel_path,
        ],
    )
}

fn test() -> Result<(), String> {
    run_command(&cargo_program(), &["test"])
}

fn check() -> Result<(), String> {
    let cargo = cargo_program();
    run_command(&cargo, &["fmt", "--all", "--", "--check"])?;
    run_command(&cargo, &["clippy", "--", "-D", "warnings"])?;

    let mut bare_metal_arguments = vec!["clippy", "--target", KERNEL_TARGET];
    for package in BARE_METAL_PACKAGES {
        bare_metal_arguments.extend(["--package", package]);
    }
    bare_metal_arguments.extend(["--", "-D", "warnings"]);
    run_command(&cargo, &bare_metal_arguments)
}
