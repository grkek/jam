# 0004 Messages and system call status

Status: **Accepted**

## Decision

A message is a header, payload bytes, and address slots, in that order.

- The kernel never parses the payload. Addresses travel in separate slots, so the
  kernel can validate and rewrite exactly those. The payload refers to an address
  by slot number.
- The kernel stamps `sender_address` in the header, so a sender cannot forge it.
- Maxima: 4096 payload bytes, 8 address slots. Larger payloads will travel as shared
  immutable buffers, designed separately.
- An address serializes to 42 bytes: kind (1), node identifier (33), local address (8).
  A local address carries a zeroed node identifier, and any other encoding is rejected,
  so each address has exactly one wire form.
- Integers are little-endian.

The kernel answers in `x0` with a `SystemCallStatus`, and in `x1` with a value for calls
that produce one.

- `Accepted`, `UnknownSystemCall`, `PermissionDenied`, `BudgetExhausted`,
  `MalformedAddress`, `InvalidArgument`, `TimedOut`.
- Operating on an actor that has already ended (`tell_actor`, `terminate`, `watch_actor`)
  is accepted silently, so callers cannot probe whether an actor is alive.
- `MalformedAddress` means the address itself is invalid, which says nothing about
  liveness. It replaces the earlier proposal `NoSuchAddress`.

## Unchanged

- `Cause::Terminated` counts as an abnormal ending and travels along tethers.
