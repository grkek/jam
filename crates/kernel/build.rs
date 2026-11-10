//! Configures kernel linking and the Genesis ELF embedded during compilation.
//!
//! Build orchestration, firmware downloads, image creation, and QEMU execution
//! belong to `tools/xtask`. This script never launches Cargo or downloads files.

use std::{env, path::PathBuf, process::ExitCode};

/// Linker script for QEMU's AArch64 virtual board.
const VIRTUAL_LINKER_SCRIPT: &str = "linker/aarch64_virt.ld";
/// Linker script for the Raspberry Pi 4 firmware load address.
const RASPBERRY_PI_4_LINKER_SCRIPT: &str = "linker/aarch64_raspberry_pi_4.ld";

/// Selects the linker script and validates the Genesis input supplied by xtask.
fn configure_compilation() -> Result<(), String> {
    println!("cargo:rerun-if-changed={VIRTUAL_LINKER_SCRIPT}");
    println!("cargo:rerun-if-changed={RASPBERRY_PI_4_LINKER_SCRIPT}");
    println!("cargo:rerun-if-env-changed=GENESIS_ELF_PATH");

    let manifest_directory = env::var_os("CARGO_MANIFEST_DIR")
        .map(PathBuf::from)
        .ok_or("Cargo did not supply CARGO_MANIFEST_DIR")?;
    let linker_script = if env::var_os("CARGO_FEATURE_RASPBERRY_PI_4").is_some() {
        RASPBERRY_PI_4_LINKER_SCRIPT
    } else {
        VIRTUAL_LINKER_SCRIPT
    };
    if env::var("CARGO_CFG_TARGET_ARCH").as_deref() == Ok("aarch64") {
        let linker_path = manifest_directory.join(linker_script);
        if !linker_path.is_file() {
            return Err(format!(
                "linker script is missing: {}",
                linker_path.display()
            ));
        }
        println!("cargo:rustc-link-arg-bins=-T{}", linker_path.display());
    }

    let genesis_program = env::var_os("GENESIS_ELF_PATH")
        .map(PathBuf::from)
        .ok_or("GENESIS_ELF_PATH is missing; use `cargo xtask build` or `cargo xtask check`")?;
    let genesis_program = genesis_program.canonicalize().map_err(|error| {
        format!(
            "cannot locate Genesis {}: {error}",
            genesis_program.display()
        )
    })?;
    if !genesis_program.is_file() {
        return Err(format!(
            "Genesis is not a file: {}",
            genesis_program.display()
        ));
    }
    let genesis_path = genesis_program
        .to_str()
        .ok_or("the Genesis ELF path must be valid UTF-8")?;
    println!("cargo:rerun-if-changed={genesis_path}");
    println!("cargo:rustc-env=GENESIS_ELF_PATH={genesis_path}");
    println!("cargo:rerun-if-env-changed=TERMINAL_ELF_PATH");
    if env::var_os("CARGO_FEATURE_STRESS").is_none() {
        let terminal = env::var_os("TERMINAL_ELF_PATH")
            .map(PathBuf::from)
            .ok_or("TERMINAL_ELF_PATH is missing; use cargo xtask build")?;
        let terminal = terminal
            .canonicalize()
            .map_err(|error| format!("cannot locate terminal: {error}"))?;
        if !terminal.is_file() {
            return Err("terminal ELF is not a file".into());
        }
        let terminal = terminal.to_str().ok_or("terminal ELF path must be UTF-8")?;
        println!("cargo:rerun-if-changed={terminal}");
        println!("cargo:rustc-env=TERMINAL_ELF_PATH={terminal}");
        println!("cargo:rerun-if-env-changed=FILESYSTEM_ELF_PATH");
        let filesystem = env::var_os("FILESYSTEM_ELF_PATH")
            .map(PathBuf::from)
            .ok_or("FILESYSTEM_ELF_PATH is missing; use cargo xtask build")?;
        let filesystem = filesystem
            .canonicalize()
            .map_err(|error| format!("cannot locate filesystem: {error}"))?;
        if !filesystem.is_file() {
            return Err("filesystem ELF is not a file".into());
        }
        let filesystem = filesystem
            .to_str()
            .ok_or("filesystem ELF path must be UTF-8")?;
        println!("cargo:rerun-if-changed={filesystem}");
        println!("cargo:rustc-env=FILESYSTEM_ELF_PATH={filesystem}");
    }
    Ok(())
}

/// Reports configuration errors without starting any build orchestration.
fn main() -> ExitCode {
    match configure_compilation() {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("kernel build: {message}");
            ExitCode::FAILURE
        }
    }
}
