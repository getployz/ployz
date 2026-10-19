---
name: verify-volume-runs
description: Model-check Volume Runs (Move, Release, late steps from a dead run) against DESIGN.md bet 6 with the Stateright model in core/crates/ployz-verify. Use when a change touches `plan` or `expect_matches` in ployz-core's volume_copy.rs, `SetVolumeCopy` handling in ployzd (local_volume_copy.rs, spec_store.rs floors, container.rs cut and start), or the volume-run Inngest steps and Postgres gates in dashboard/src/modules/volume-run/.
---

# Verify Volume Runs

The model in `core/crates/ployz-verify/src/volume_runs.rs` explores every interleaving of one Move, the Release that takes it over, and every late `SetVolumeCopy` the dead Move can still deliver. It calls the real `ployz_core::plan`, so a planner change is checked as written. Everything else (the Machine's lock and floor steps, the Inngest drivers, the Postgres commit gate) is a hand copy of the code, and drifts unless you keep it in step.

## Run it

```
cd core && cargo test -p ployz-verify
```

About 30 s with the build, 6 s warm. CI runs it in the workspace nextest, so a red model blocks the PR.

The run is green when all five tests pass:

- `volume_runs_hold`: the code as it is. Every property holds and every `sometimes` is reached. A `sometimes` that is never reached also fails here: it means the model can no longer reach that outcome, so a property passing beside it proves nothing.
- The four `breaks` tests each remove one guard (a `Broken` flag) and expect the named properties to fail. One of these going green-to-red means a guard no longer earns its keep, or the model lost the path that shows it.

## Read a counterexample

A failure prints the property name and a `Path[n]` of actions:

- `Move`, `Release`: the driver takes its next step, sending a request when the step talks to a Machine.
- `MoveResend`, `ReleaseResend`: the driver resends its outstanding request; the earlier copy may still land later.
- `EndMove`: Cloud ends the Move's row, which spawns the Release.
- `Machine(i)`: request `i` in `State.requests` advances one step (admit, cut drain, stop, flip, start, reply). The index is the request's position at that moment, not a stable id.
- `Crash(i)`: A's daemon restarts mid-flip of request `i`, losing every in-flight request on that Machine.

The action list alone rarely tells the story. To see the states, add a scratch test that takes the path and prints it, then delete it:

```rust
let path = check(VolumeRuns::default()).discovery(DONE_HOLDS).expect("found");
for (state, action) in path.into_vec() { println!("{state:#?}\n-> {action:?}"); }
```

Write the story out in Machine terms (which request, which gen, what the floor was, what `plan` returned) before deciding anything.

## Real bug or model error

A counterexample is a **lead**, not a verdict. Confirm each step of the story against the code it models before calling it a bug. The model has been wrong before: it once let A's daemon crash mid-flip even when the flip's own floor check would have refused, so a Release appeared to fail on correct code. The fix was gating `Crash` on the same checks the flip makes (`fn flips`).

- If a step does something the code cannot, fix the model and say so.
- If every step matches the code, it is a finding. Report it in plain words with the path. Keep the property and the model as they are; weakening either to go green hides the bug.

## A break that stays green

Guards overlap, so removing one can leave every property holding. Before calling a guard dead, rerun the change with a neighbouring `Broken` flag set too, in a scratch test that prints `checker.discovery(name)` for each property. Removing the epoch comparison in `expect_matches` stays green on its own, because the floors already refuse every late request it would stop. With `floors` broken as well, it fails "a run that finished done still holds at rest". So the epoch check is the second fence behind the floors.

## Keep the model in step

When a change intentionally alters the protocol (a new guard, a new step, a reordered check), change the model in the same PR:

1. Update the matching model step, citing the file and function it copies.
2. If the change adds a guard, add a `Broken` flag that removes it and a `breaks` test naming the property it keeps. A guard with no failing variant is untested.
3. If a property's meaning changes, change its text and say why in the PR body. A property narrows only when the old one was wrong about the design, as `RELEASE_WINS` was for forward Releases, where a dead Move's own start on B legitimately lands first.

## Bounds and blind spots

Bounds: Machines A and B, one Move and one takeover, one resend per run, one daemon crash, one Busy retry. A bug that needs more of any of these is invisible.

Out of scope: Deploy lanes, operator collisions, Machines that never answer, warm rounds, a second takeover, and anything inside `plan`'s callers that the model does not copy. A green model says nothing about them; cover them with tests or `verify-server`.
