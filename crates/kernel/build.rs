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
            return Err(format!("linker script is missing: {}", linker_path.display()));
        }
        println!("cargo:rustc-link-arg-bins=-T{}", linker_path.display());
    }

    let genesis_program = env::var_os("GENESIS_ELF_PATH")
        .map(PathBuf::from)
        .ok_or("GENESIS_ELF_PATH is missing; use `cargo xtask build` or `cargo xtask check`")?;
    let genesis_program = genesis_program
        .canonicalize()
        .map_err(|error| format!("cannot locate Genesis {}: {error}", genesis_program.display()))?;
    if !genesis_program.is_file() {
        return Err(format!("Genesis is not a file: {}", genesis_program.display()));
    }
    let genesis_path = genesis_program
        .to_str()
        .ok_or("the Genesis ELF path must be valid UTF-8")?;
    println!("cargo:rerun-if-changed={genesis_path}");
    println!("cargo:rustc-env=GENESIS_ELF_PATH={genesis_path}");
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
