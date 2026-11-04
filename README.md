<p align="center">
  <a href="https://github.com/grkek/jam">
    <img src="assets/banner.svg" alt="JAM" />
  </a>
</p>

JAM, a.k.a. Just Another Microkernel, is a distributed operating fabric written in Rust for AArch64. It is built around the actor concurrency model and inspired by the resilience of Erlang. Dedicated to the memory of Joe Armstrong.

## Layout

| Path | Purpose |
|---|---|
| `crates/foundation` | Shared types: addresses, causes, farewells, resource budgets, and the system call ABI. No `unsafe`; testable on the host. |
| `crates/kernel` | The microkernel, with architecture-specific code in `architecture/aarch64` and board-specific code in `platform/`. |
| `crates/runtime` | Actor runtime support. |
| `services/` | Userland actors: genesis, registry, observatory, distribution, and drivers. |
| `tools/xtask` | Build and run automation. |
| `documentation/` | Architecture notes and decision records. |
| `tests/` | Host and QEMU tests. |

## Usage

Install the toolchain specified in `rust-toolchain.toml`, then:

```sh
rustup target add aarch64-unknown-none-softfloat
rustup component add llvm-tools
cargo install cargo-binutils
```

```sh
cargo test                                    # host-side tests
cargo xtask build                             # build every board
cargo xtask build --board virtual             # build QEMU virtual board
cargo xtask build --board raspberry-pi-4      # build Raspberry Pi 4
cargo xtask build --release                   # build every board in release mode
cargo xtask run                               # boot QEMU virtual board with eight cores
cargo xtask run-raspberry-pi-4                # boot QEMU on Raspberry Pi 4 board
```

Build output goes into `target/<board>/`.

The Raspberry Pi build automatically clones its firmware into
`target/raspberry-pi-4/firmware` and reuses that checkout on later builds.
The first build requires Git and an internet connection. QEMU `virt` does
not need a firmware checkout.

To boot a Raspberry Pi 4, copy `kernel8.img`, `config.txt`, `start4.elf`,
`fixup4.dat`, `bcm2711-rpi-4-b.dtb`, and `overlays/` from
`target/raspberry-pi-4/` to a FAT32 boot partition on an SD card.
The `firmware/` checkout itself does not belong on the card.

### Stress test

The genesis program doubles as a messaging stress test. At boot the kernel starts
dozens of actors that flood, fuzz, crash and churn scenarios.

CI boots both boards and checks that no actor was stranded, no dead address accepted
a message, and the number of reported actor faults equals the number designed in.
Run the same check locally:

```sh
bash tools/ci/boot-test.sh boot.log 120 cargo xtask run
bash tools/ci/boot-test.sh boot.log 120 cargo xtask run-raspberry-pi-4
```

## Contribute

See the [contribution guidelines](https://github.com/grkek/jam/blob/master/CONTRIBUTING.md)
and [code of conduct](https://github.com/grkek/jam/blob/master/CODE_OF_CONDUCT.md).