#!/usr/bin/env bash
set -euo pipefail

# Run from the root of the JAM repository.
# This script does not modify GitHub or install firmware on the SD card.

TARGET_TRIPLE="aarch64-unknown-none-softfloat"
OUTPUT_DIRECTORY="target/raspberry-pi-4"
KERNEL_EXECUTABLE="target/${TARGET_TRIPLE}/release/kernel"

cargo build --release --package genesis --target "${TARGET_TRIPLE}"
cargo build --release --package kernel --target "${TARGET_TRIPLE}" --features raspberry-pi-4

if command -v rust-objcopy >/dev/null 2>&1; then
    OBJECT_COPY_PROGRAM="rust-objcopy"
elif command -v llvm-objcopy >/dev/null 2>&1; then
    OBJECT_COPY_PROGRAM="llvm-objcopy"
else
    printf '%s\n' 'Missing llvm-objcopy (install llvm-tools or add llvm-objcopy to PATH).' >&2
    exit 1
fi

mkdir -p "${OUTPUT_DIRECTORY}"
"${OBJECT_COPY_PROGRAM}" --output-target=binary "${KERNEL_EXECUTABLE}" "${OUTPUT_DIRECTORY}/kernel8.img"
printf 'Created %s/kernel8.img\n' "${OUTPUT_DIRECTORY}"
