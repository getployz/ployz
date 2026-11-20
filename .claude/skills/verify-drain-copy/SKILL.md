---
name: verify-drain-copy
description: Model-check a drain's start of its new copy on the destination Server (the slot key the move mints once, the handler's plan, recovery of a half-made Container, create or adopt, image check by ID, start and final read, other ensure.v1 callers in `move:{old}` and a second drain's slot, a Machine restore, and the drain's cancel, resend, one Gone by its key, and its report) with the Stateright model in core/crates/ployz-verify/src/drain_copy.rs. Use when a change touches converge_now, create_and_start, create_refusal, image_differs or pinned in core/crates/ployzd/src/machine/local_machine/container.rs, matching_creation, create_with_admission or create_admitted in core/crates/ployzd/src/docker/lifecycle.rs, ensure, ensure_pinned_container or pinned_shape in core/crates/ployzd/src/machine_api/local.rs, move_container, Started, emptied or MoveEnd in core/crates/ployz/src/deploy/exec/start.rs, ensure_new in core/crates/ployz/src/deploy/exec/machine_ops.rs, or RestartTolerant, Outstanding, ensure_new_noting or the ensure_new row in core/crates/ployz/src/deploy/exec/transport.rs.
---

# Verify drain copy

The model in `core/crates/ployz-verify/src/drain_copy.rs` explores every interleaving of one drain starting its new copy on the destination. Four actors take turns, one step at a time:

- **the handler** (`MachineService::ensure`, then `converge_now` and `create_and_start`) reads the slot its request names. It removes a half-made Container of that key (`matching_creation`), then creates the Container, or adopts one a name conflict finds when it has the same spec. It checks the image by ID, starts, reads the Container it acted on, and answers. Any step may end in an error instead. It runs once, and again after a lost answer or a first send the entry couldn't dial, unless the entry refuses to dial again for the resend.
- **the planter**, an honest other caller (an older CLI's move, a hand-written ensure.v1, a second drain), acts at most twice in `move:{old}` or in a second drain's minted slot. It creates a Container of either spec and either image, half-made or started, stops one, or removes one by key. It never acts in this move's minted slot. Only a forger can name that slot, which is an accepted residual.
- **a Machine restore**, once, gives the move's own Container a new ID under the same key. The move stays its creator.
- **the drain** (`move_container`) mints its slot once, sends, may cancel at any point (the resend wait included), and waits out health. After any end but a healthy Container it sends one Gone by its slot's key, resent while the Machine restarts. It reports the replacement removed only when that Gone read the slot back empty and no send went out without an answer (`Outstanding`).

The drain's cleanup has no branch, so the model calls no production decision. Every step is a hand copy of the code it names, so it drifts unless you keep it in step.

The five properties:

- **The reply is ours.** Every `Present` answer names the Container the last run made or adopted, from the drain's spec, on the pinned image.
- **No wrong start.** The handler never starts a stopped Container that runs another image.
- **No foreign removal.** Every Container that a removal the drain caused took was made by one of the move's sends. That covers the drain's Gone by key and the handler's own recovery of a half-made Container.
- **Nothing of ours is left.** A drain that ends without moving leaves no Container any of its sends made, in any slot, unless the handler made that Container after the Gone.
- **The report is true.** A drain that reports its replacement removed leaves no Container its sends made.

The harm when one breaks: a drain reports a move that runs another image, starts a Container another caller left, deletes another caller's Container, leaves its own Pending Container behind, or reports clean while one stays.

## Run it

```
cd core && cargo test -p ployz-verify drain_copy -- --nocapture
```

The run takes under a second, and the shipped model has about 209,000 states. CI runs it in the workspace nextest.

Green means all six tests pass:

- `the_drain_s_cleanup_takes_only_what_its_own_sends_made` checks the code as it ships. Every property holds, and every `sometimes` is reached: a fresh answer, a resend that adopts what its lost send made and a Gone that removes it, a cancel in the resend wait beside another caller's Container in `move:{old}`, a Gone that removes a Container a restore gave a new ID, a late create after the Gone, and a report of the replacement removed.
- Five tests each switch one guard back to an earlier design and expect its counterexample:
  - `Key::Shared` names `move:{old}` on every send. A cancel, then another caller's Container in that slot, and the Gone takes it. This breaks "no foreign removal".
  - `Key::PerSend` mints a key for each send. The first send makes a Container and loses its answer, the resend names a new slot, and the Gone by that slot leaves the first. This breaks "nothing of ours is left".
  - `ReplyBy::Slot` reads the slot instead of the acted ID, which breaks "the reply is ours" (under `Key::Shared`, where another caller can fill the slot).
  - `Check::None` breaks both "the reply is ours" and "no wrong start".
  - `Check::AfterStart` breaks "no wrong start" with another caller's stopped Container.

## Read a counterexample

Each row is one step in the code's words, then every slot's Container, the handler's run, slot, step and acted ID, and the drain's phase, answer, `Outstanding` bit and Gone. Confirm each row against the function it names before you call it a bug. A step the code cannot take is a model error: fix the model and say so. If every step matches, report it with the path. Never weaken a property to turn the run green.

## Keep the model in step

When a change alters the order of the handler's steps, what the final read names, which key a send names, or how the drain cleans up or reports:

1. Change the matching `Step` arm, `answer`, `slot_of` or `Cleanup` arm, citing the function it copies.
2. A new guard gets a switch that removes it, and a test that expects its counterexample.
3. Change a property's text only when the design changed, and say why in the PR.

## Bounds and blind spots

Bounds: four slots, Container IDs up to 5, two specs, two images, one resend.

The model leaves these out:

- a second destination;
- a forger that names this move's minted key;
- a resent Gone, which removes nothing the first did not, and a Gone that reads back a Container, which the drain reports as not removed;
- rotation and every step after a healthy answer (`relocate`, `bring_in`, `wait_listed`, `retire_old`), the rerun that takes over a promoted copy included;
- who signed the spec, and the peer image copy.

A send still out when the Gone runs can make its Container after it. The model reaches that case, exempts that Container from "nothing of ours is left", and the drain's report says it may still be there.

Sync point: main at `65d9e5c65a`, plus the change that mints the move's key once and cleans up with one Gone by it.
