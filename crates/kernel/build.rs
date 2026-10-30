//! Selects the target board's linker script and locates the embedded Genesis ELF.
//!
//! QEMU `virt` remains the default. Enabling `raspberry-pi-4` selects the
//! BCM2711 firmware load address and the separate Raspberry Pi boot assembly.

use std::{env, fs, path::PathBuf};

const QEMU_LINKER_SCRIPT: &str = "linker/aarch64_virt.ld";
const RASPBERRY_PI_4_LINKER_SCRIPT: &str = "linker/aarch64_raspberry_pi_4.ld";

fn main() {
    println!("cargo:rerun-if-changed={QEMU_LINKER_SCRIPT}");
    println!("cargo:rerun-if-changed={RASPBERRY_PI_4_LINKER_SCRIPT}");

    let raspberry_pi_4_enabled = env::var_os("CARGO_FEATURE_RASPBERRY_PI_4").is_some();
    let linker_script = if raspberry_pi_4_enabled {
        RASPBERRY_PI_4_LINKER_SCRIPT
    } else {
        QEMU_LINKER_SCRIPT
    };

    let Ok(manifest_directory) = env::var("CARGO_MANIFEST_DIR") else {
        return;
    };

    if env::var("CARGO_CFG_TARGET_ARCH").as_deref() == Ok("aarch64") {
        println!("cargo:rustc-link-arg-bins=-T{manifest_directory}/{linker_script}");
    }

    let Ok(output_directory) = env::var("OUT_DIR") else {
        return;
    };
    let output_directory = PathBuf::from(output_directory);

    // OUT_DIR is `<target>/<triple>/<profile>/build/kernel-<hash>/out`.
    let genesis_elf_path = output_directory
        .ancestors()
        .nth(3)
        .map(|profile_directory| profile_directory.join("genesis"))
        .filter(|path| path.is_file());

    let genesis_elf_path = genesis_elf_path.unwrap_or_else(|| {
        println!(
            "cargo:warning=Genesis was not built; embedding an empty stand-in. Build Genesis first."
        );
        let stand_in_path = output_directory.join("genesis_stand_in.elf");
        if let Err(error) = fs::write(&stand_in_path, []) {
            eprintln!("cannot create the empty Genesis stand-in: {error}");
            std::process::exit(1);
        }
        stand_in_path
    });

    println!("cargo:rerun-if-changed={}", genesis_elf_path.display());
    println!(
        "cargo:rustc-env=GENESIS_ELF_PATH={}",
        genesis_elf_path.display()
    );
}
