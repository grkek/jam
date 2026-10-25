//! Hands the linker script to the linker when building for AArch64.

use std::env;

fn main() {
    println!("cargo:rerun-if-changed=linker/aarch64_virt.ld");
    if env::var("CARGO_CFG_TARGET_ARCH").as_deref() != Ok("aarch64") {
        return;
    }
    let Ok(manifest_directory) = env::var("CARGO_MANIFEST_DIR") else {
        return;
    };
    println!("cargo:rustc-link-arg-bins=-T{manifest_directory}/linker/aarch64_virt.ld");
}
