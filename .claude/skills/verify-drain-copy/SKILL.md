---
name: verify-drain-copy
description: Model-check a drain's start of its new copy on the destination Server (the handler's plan, create or adopt, image check by ID, start and final read, another ensure.v1 caller on the same slot key, and the drain's cancel, resend and cleanup by answer class) with the Stateright model in core/crates/ployz-verify/src/drain_copy.rs. Use when a change touches converge_now, create_and_start, image_differs or pinned in core/crates/ployzd/src/machine/local_machine/container.rs, ensure_pinned_container, pin_on_ensure or pinned_shape in core/crates/ployzd/src/machine_api/local.rs, move_container, Leftover or MoveEnd in core/crates/ployz/src/deploy/exec/start.rs, ensure_new in core/crates/ployz/src/deploy/exec/machine_ops.rs, or the ensure_new row in core/crates/ployz/src/deploy/exec/transport.rs.
---

# Verify drain copy

The model in `core/crates/ployz-verify/src/drain_copy.rs` explores every interleaving of one drain starting its new copy on the destination. Three actors take turns, one step at a time:

- **the handler** (`converge_now` and `create_and_start`) plans from the slot, creates or adopts what a name conflict finds, checks the image by ID, starts, reads the Container it acted on, and answers. It runs once, and again after a lost answer;
- **the planter**, another ensure.v1 caller on the drain's slot key `move:{old}`, acts at most twice. It creates and starts a Container of either spec and either image, stops the slot's Container, or removes it by key;
- **the drain** (`move_container`) sends, may cancel before the answer, waits out health, and cleans up by its `Leftover`.

Nothing calls the real code. The model is a hand copy, so it drifts unless you keep it in step.

The three properties:

- **The reply is ours.** Every `Present` answer names the Container this run made or adopted, from the drain's spec, on the pinned image.
- **No wrong start.** The handler never starts a stopped Container that runs another image.
- **Cleanup by the named ID.** After an answer, the drain removes only the Container a `Present` answer named. After `Absent` or a refusal it removes nothing. It removes by key only when no answer came.

The harm when one breaks: a drain reports a move that runs another image, starts a Container another caller left, or deletes a Container that was never its own.

## Run it

```
cd core && cargo test -p ployz-verify drain_copy -- --nocapture
```

The run takes well under a second, and the shipped model has about 1,500 states. CI runs it in the workspace nextest.

Green means all six tests pass:

- `the_drain_s_start_answers_and_cleans_up_only_its_own_container` checks the code as it ships. Every property holds, and every `sometimes` is reached: a fresh answer, a resend that adopts its own Container, an `Absent` answer beside the planter's Container, a refusal that leaves the planter's occupant, and a late create after a removal by key.
- Five tests each switch one guard back to an earlier design and expect its counterexample:
  - `ReplyBy::Slot` reads the slot instead of the acted ID, which breaks "the reply is ours".
  - `Check::None` breaks both "the reply is ours" and "no wrong start".
  - `Check::AfterStart` breaks "no wrong start" with the planter's stopped Container.
  - `Cleanup::BySlot` is main's discard after any failure. It breaks "cleanup by the named ID" through an `Absent` answer.
  - `Cleanup::RefusedOccupant` removes the occupant a refusal names, which also breaks "cleanup by the named ID".

## Read a counterexample

Each row is one step in the code's words, then the slot, the handler's run, step and acted ID, and the drain's phase, answer and removal. Confirm each row against the function it names before you call it a bug. A step the code cannot take is a model error: fix the model and say so. If every step matches, report it with the path. Never weaken a property to turn the run green.

## Keep the model in step

When a change alters the order of the handler's steps, what the final read names, or how the drain cleans up:

1. Change the matching `Step` arm, `answer`, or `removal`, citing the function it copies.
2. A new guard gets a switch that removes it, and a test that expects its counterexample.
3. Change a property's text only when the design changed, and say why in the PR.

## Bounds and blind spots

Bounds: one destination slot, Container IDs up to 4, two specs, two images, one resend.

The model leaves these out:

- a second destination;
- two drains at once;
- rotation and every step after a healthy answer (`relocate`, `bring_in`, `wait_listed`, `retire_old`);
- who signed the spec, and the peer image copy.

A removal by key with no answer can still take another caller's Container that filled the slot meanwhile. The model reaches that case and accepts it, as main does.

Sync point: main at `998b068c2e`, plus the change that added this model.
