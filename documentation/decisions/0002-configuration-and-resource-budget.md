# 0002 Configuration and ResourceBudget

Status: **Accepted**

## Decision

- `Configuration` is everything chosen at spawn: `ResourceBudget`, `OverflowPolicy`,
  `Lineage`, `Survival`.
- Struct names are singular.
- Lineage and survival are fixed at birth. Budget and overflow policy can be adjusted.
- A parent may set any budget on its children. An actor may only lower its own budget,
  never raise it (`ResourceBudget::fits_within`).
- Testimony is capped at 1 KiB by the kernel, independent of any budget.
- Spawning tethered is `Configuration { lineage: Lineage::Tethered, .. }` on a single
  `SpawnActor` call, replacing the separate `spawn_tethered` call.
