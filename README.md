<div align="center" style="margin: 30px 0;">
  <div style="background-color: #f5da55; padding: 18px 45px; display: inline-block; border-radius: 4px;">
    <span style="font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', Roboto, Helvetica, Arial, sans-serif; font-weight: 900; font-size: 54px; color: #000000; letter-spacing: 2px; line-height: 1;">
      JAM
    </span>
  </div>
</div>

JAM a.k.a Just Another Microkernel is a fault-tolerant, actor-based software for AArch64, written in Rust. It is inspired by the resilience of Erlang and dedicated to the memory of Joe Armstrong.

## Layout

| Path | Purpose |
|---|---|
| `crates/foundation` | Shared vocabulary: addresses, causes, farewells, limits, system call ABI. No `unsafe`, testable on the host. |
| `crates/kernel` | The microkernel. Architecture-specific code lives only in `architecture/aarch64`. |
| `crates/runtime` | Library linked into every actor: inbox, typed messages, allocator, panic handling. |
| `services/` | Ordinary actors: genesis, registry, observatory, distribution, drivers. |
| `tools/xtask` | Build, run, and test automation (`cargo xtask <command>`). |
| `documentation/` | Architecture notes and numbered decision records. |
| `tests/` | Host tests and QEMU boot-and-assert tests. |

## Usage

```
cargo test           # host-side tests (foundation)
cargo xtask -- build # build everything for aarch64-unknown-none-softfloat
cargo xtask -- run   # boot on QEMU virt with -smp 4
```

## Contribute

See our [contribution guidelines](https://github.com/your-username/jam/blob/master/CONTRIBUTING.md) and read the [code of conduct](https://github.com/your-username/jam/blob/master/CODE_OF_CONDUCT.md).
