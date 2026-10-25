# 0001 Vocabulary

Status: **Accepted**

## Decision

| Concept | Name |
|---|---|
| Unit of execution and isolation | `Actor` |
| Reference to an actor (locality explicit) | `Address` (`Local` / `Remote`) |
| Per-actor message store | `Inbox` |
| One-way delivery | `tell_actor` |
| Selective receive | `await_matching` |
| Observation of another actor's end | `watch_actor` |
| Notice delivered to watchers | `Farewell { actor, cause, testimony }` |
| Why an actor ended | `Cause` |
| Actor-authored, bounded explanation | `Testimony` |
| Full kernel record of an end (parent, Genesis, Observatory only) | `Autopsy` |
| Failure link created at birth only | `spawn_tethered` |
| Receives tethered failures as farewells instead of falling | `become_guardian` |
| Restart-policy actor (userspace pattern) | `Steward` |
| Root actor | `Genesis` |
| Process table snapshots | `Census` |
| Opt-in event trace | `Chronicle` |
| Always-on flight recorder | `Blackbox` |
| Introspection actor | `Observatory` |

## Rules

- Termination authority: parent and Genesis only.
- Tethers: created at spawn only. Watching: open to everyone.
- Parent departure cascades to children unless spawned detached.
- Delivery to a dead actor is silently discarded. Messages sent before an
  actor's end are delivered before its farewell.
