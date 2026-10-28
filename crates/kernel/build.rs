//! Hands the linker script and genesis ELF path to the linker when building for AArch64.

use std::env;
use std::path::PathBuf;

fn main() {
    println!("cargo:rerun-if-changed=linker/aarch64_virt.ld");
    println!("cargo:rerun-if-changed=../services/genesis");

    let Ok(manifest_directory) = env::var("CARGO_MANIFEST_DIR") else {
        return;
    };

    let linker_script_path = format!("{manifest_directory}/linker/aarch64_virt.ld");
    println!("cargo:rustc-link-arg-bins=-T{linker_script_path}");

    let profile = env::var("PROFILE").unwrap_or_else(|_| "debug".to_string());

    // Always point to the bare-metal target directory for genesis
    let target_triple = "aarch64-unknown-none-softfloat";
    let workspace_root = PathBuf::from(&manifest_directory).join("../..");

    let genesis_elf_path = workspace_root
        .join("target")
        .join(target_triple)
        .join(profile)
        .join("genesis");

    println!(
        "cargo:rustc-env=GENESIS_ELF_PATH={}",
        genesis_elf_path.display()
    );
}
