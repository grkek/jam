# 0005 Scrolls

Status: **Accepted**

A Scroll is an immutable, reference-counted byte buffer in a shared pool that the kernel
owns. Actors offload large data to it so they do not hold it themselves, and pass it
between actors by reference. Actors are not charged for holding one.

## The pool

- Grows in chunks on demand and returns empty chunks to the frame allocator.
- Never exceeds a **pool ceiling** set at boot (default: a quarter of RAM). At the
  ceiling, creating a Scroll fails with `BudgetExhausted`. Nothing is evicted and no
  actor is ended.
- A **reserve** of memory is always left for actor heaps and the kernel, so Scrolls can
  never take the last of RAM.
- A **maximum size per Scroll** is set at boot, so one allocation cannot absorb the pool.

## Lifecycle

1. `CreateScroll` returns a draft with a writable view (never executable).
2. `SealScroll` unmaps the writable view, remaps the pages read-only, and invalidates
   TLB entries with the broadcast `tlbi ... is`. A sealed Scroll can never be written again.
3. A reference is placed in a message slot. Sending takes a reference for the receiver;
   the receiver maps the Scroll read-only with `MapScroll`.
4. `ReleaseScroll`, or the holder ending, drops the reference. At zero the memory is
   zeroed and returned to the pool.

## Security

- **Pages are never executable.** `UXN` and `PXN` are set on every Scroll page. Writable
  and executable are never true together, and after sealing nothing is writable.
- **References are per-actor handles**, validated by the kernel and carrying a generation
  counter. A forged or stale handle is rejected with `InvalidScrollReference`. A reference
  inside a message is rewritten for the receiver.
- **Pure bytes.** A Scroll holds no addresses or Scroll references, so cycles are
  impossible and a plain counter is enough.
- **Memory is zeroed before reuse**, so data never leaks between actors.
- **Guard pages** surround each mapping in an actor, so an out-of-bounds read faults.
- **The count never overflows.** At its maximum, sending another reference fails with
  `BudgetExhausted`.

## Fault tolerance

- **The kernel tracks every reference.** An actor that ends, for any `Cause`, has all of its
  references released as part of teardown, and its unsealed drafts freed.
- **Messages discarded on an actor's end release their Scroll references**, whether they
  sat in the staging queue or the inbox.
- **Releasing happens exactly once**, because the kernel's table decides it, not the
  actor's honesty.
- **Heavy Scroll use cannot take down the system.** The ceiling, the reserve, and the
  per-Scroll maximum bound the damage. The census shows pool size, free space, and which
  actors hold the most references, so a steward can act on a runaway holder.

## Messages

A slot is now an address or a Scroll reference. `MessageHeader` counts `slot_count` slots
(maximum 8), all 42 bytes on the wire. Scroll slots use kind 2, and any other padding
is rejected so each slot has one encoding.

Sending to a `RemoteAddress` copies the bytes, and the remote node creates its own Scroll.

## Per-actor caps

- At most 256 Scroll references and 8 unsealed drafts per actor
  (`SCROLL_REFERENCE_MAXIMUM_PER_ACTOR`, `SCROLL_DRAFT_MAXIMUM_PER_ACTOR`).
- These bound the kernel's bookkeeping, not memory, so they are fixed system constants
  and not charged against the resource budget. Exceeding one returns `BudgetExhausted`.
- The numbers are first guesses. Tune them once the kernel exists.

## Slicing

- `SliceScroll(reference, offset, length)` returns a new `ScrollReference` to a byte range
  of a sealed Scroll, without copying. Drafts cannot be sliced.
- The offset and length live in the holder's kernel reference table, so `ScrollReference`
  and the slot wire format are unchanged. A slice sent in a message stays a slice.
- A slice keeps the whole Scroll alive and counts against the reference cap. A range
  outside the Scroll returns `InvalidArgument`.
- Over the network only the sliced range is copied.
- Known cost: a tiny slice can keep a huge Scroll in the pool. The census shows pool
  holders, so a steward can see it. Copying out a small range is always an option.
