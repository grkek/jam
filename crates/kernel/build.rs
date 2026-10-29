//! Hands the linker script to the linker, and tells the kernel where the Genesis ELF is.
//!
//! `cargo xtask build` builds Genesis first. When the file is missing, an empty stand-in is used
//! so that checking and editing still work, and a warning says so.

use std::{env, fs, path::PathBuf};

fn main() {
    println!("cargo:rerun-if-changed=linker/aarch64_virt.ld");

    let Ok(manifest_directory) = env::var("CARGO_MANIFEST_DIR") else {
        return;
    };
    if env::var("CARGO_CFG_TARGET_ARCH").as_deref() == Ok("aarch64") {
        println!("cargo:rustc-link-arg-bins=-T{manifest_directory}/linker/aarch64_virt.ld");
    }

    let Ok(out_directory) = env::var("OUT_DIR") else {
        return;
    };
    let out_directory = PathBuf::from(out_directory);

    // OUT_DIR is `<target>/<triple>/<profile>/build/kernel-<hash>/out`, and Genesis is built into
    // `<target>/<triple>/<profile>`, whatever the profile or target directory is called.
    let genesis_elf_path = out_directory
        .ancestors()
        .nth(3)
        .map(|profile_directory| profile_directory.join("genesis"))
        .filter(|path| path.is_file());

    let genesis_elf_path = genesis_elf_path.unwrap_or_else(|| {
        println!("cargo:warning=Genesis has not been built; embedding an empty stand-in. Use `cargo xtask build`.");
        let stand_in = out_directory.join("genesis_stand_in.elf");
        let _ = fs::write(&stand_in, []);
        stand_in
    });

    println!("cargo:rustc-env=GENESIS_ELF_PATH={}", genesis_elf_path.display());
}
