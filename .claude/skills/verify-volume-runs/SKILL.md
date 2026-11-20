---
name: verify-volume-runs
description: Model-check Volume Runs (resize, Move, Release, late steps from a dead run) against DESIGN.md bet 6, and each Machine's refusal of an Operator's Volume start that creates a Container, with the Stateright model in core/crates/ployz-verify/src/volume_runs.rs. Use when a change touches `plan` or `expect_matches` in ployz-core's volume_copy.rs, `SetVolumeCopy` or `SetVolumeSize` handling in ployzd (machine_api/local_volume_copy.rs, the `volume_floors` in docker/spec_store.rs, `start_volume_holder` and `cut_volume_copy` in machine/local_machine/container.rs, volume_plugin/size.rs, the pull's `raise_slot_bound` in volume_plugin/copy.rs), `admit_volume_start` in machine/local_machine/admission.rs or its call in machine_api/local.rs, `size_plan`, `SizePlan`, `SizeFacts` or `Refused` in ployz-decide's volume_size.rs, or the volume-run Inngest steps and Postgres gates in dashboard/src/modules/volume-run/.
---

# Verify Volume Runs

The model in `core/crates/ployz-verify/src/volume_runs.rs` explores every interleaving of one resize of the Volume, a grow or a shrink, one Move after it, the Release that takes the Move over, and every `SetVolumeSize` and `SetVolumeCopy` a dead run already sent landing late. Once its row ends, the Move sends nothing new: each step checks ownership first. It calls the real `ployz_core::plan`, so a planner change is checked as written. Everything else (the Machine's lock and floor steps, the start gate `admit_volume_start`, `set_size_admitted` with its shrink floor and the pull's bound raise, the Inngest drivers with `RESIZE_ORDER` and `undoOnRefusal`, `requireOwner` and the `commitMove` gate) is a hand copy of the code, and drifts unless you keep it in step.

## Run it

```
cd core && cargo test -p ployz-verify
```

A few minutes on a cold build, about 30 s warm. CI runs it in the workspace nextest, so a red model blocks the PR.

The run is green when all fifteen tests pass:

- `volume_runs_hold`: the code as it is, in each of the three scenarios (below), in both directions. It prints each scenario's state count. Every property holds and every `sometimes` is reached. A `sometimes` that is never reached also fails here: it means the model can no longer reach that outcome, so a property passing beside it proves nothing.
- `volume_runs_hold_when_machines_refuse_created_starts`: the same with each Machine's start gate closing at any moment of the Move. Cloud's runs call as an Operator, and `admit_volume_start` refuses an Operator's `SetVolumeCopy` whose start creates a Container, before any floor is read; a start of a Container the Machine kept is served. A gate that closes before the first step stands for a Machine that refuses from the start. `a_machine_refuses_every_operator_start_that_creates_a_container` takes the gate out (`Broken::start_gate`) and expects "a Machine creates no Container an Operator's Writer start names" to fail.
- The twelve `breaks` tests each remove one guard (a `Broken` flag) and expect the named properties to fail. One of these going green-to-red means a guard no longer earns its keep, or the model lost the path that shows it. Four are the Move's: `floors_let_a_release_win_over_late_steps`, `floors_and_owner_checks_keep_one_writer`, `the_cut_stops_the_service_before_its_flip` and `a_refusal_frees_the_switch_lock`. Eight are the resize's. For the grow: `a_grow_records_only_after_every_copy_holds_it`, `a_refused_grow_records_nothing`, and `a_move_needs_no_raise_of_its_own`, which shows that the pull's raise is what makes a Move to a Machine the grow never asked safe without a raise step of its own. For the shrink: `a_shrink_records_before_any_copy_lowers`, `a_shrink_lowers_the_primary_before_its_mirrors`, `a_shrink_refused_before_any_copy_changed_undoes_its_record`, and `a_refused_shrink_stops`. `a_done_resize_recorded_its_target` runs in both directions.

The model was last confirmed to describe the code at commit `48582970f957293b4e37d63c7620a5d620b4a24b`.

Each start names a direction. A grow starts with every copy and the Store at the small size and asks the large one; a shrink starts at the large size and asks the small one. The one Machine that can't take the target, if any, is `cramped`: a Pool with no room for the grow, or a copy that references too much for the shrink, whose `shrink_floor` then sits above it. The refuse order of a `SetVolumeSize` (shrink, switching, below the floor, equal, grow) is the real `ployz_decide::volume_size::size_plan`, which the model calls with the two sizes as 2 GB and 4 GB and a cramped copy referencing 2 GB, so `shrink_floor` is the real function; only the Pool's room (`NoCapacity`) is the model's own `fits`.

The full product of the resize and the Move is millions of states, so `Scenario` splits it into slices, each checked whole:

- `Resize`: the resize alone from every start, its row ended at any moment, with its resend and late requests.
- `ResizeThenMove`: a resize that runs to its own end, then the Move and its Release. The `breaks` tests run here unless they name another.
- `LateResize`: a resize whose row ends while a `SetVolumeSize` is on the wire. The Move starts at once, and the late request lands anywhere in it.

## Read a counterexample

A failure prints the property name and a `Path[n]` of actions:

- `Resize`, `Move`, `Release`: the driver takes its next step, sending a request when the step talks to a Machine. The resize's steps are `Set(A)`, `Set(B)`, `Record`, `Unrecord` and `Finish`; the state's `direction` says which way a `Set` goes.
- `ResizeResend`, `MoveResend`, `ReleaseResend`: the driver resends its outstanding request; the earlier copy may still land later.
- `EndResize`: Cloud ends the resize's row. Its `SetVolumeSize` still lands later, unfenced.
- `EndMove`: Cloud ends the Move's row, which spawns the Release.
- `Machine(i)`: request `i` in `State.requests` advances one step (admit, cut drain, stop, flip, start, reply; a `SetVolumeSize` runs in one step). The index is the request's position at that moment, not a stable id. A `SetVolumeSize` answer no driver waits on leaves the list when it lands, and the indices after it move down.
- `Crash(i)`: A's daemon restarts mid-flip of request `i`, losing every in-flight request on that Machine.
- `Enforce`: every Machine's start gate closes (only in the two start-gate tests).

The checker hands jobs out in blocks, so a path can run past the first state that breaks a property. Find the first failing step before telling the story.

The action list alone rarely tells the story. To see the states, add a scratch test that takes the path and prints it, then delete it:

```rust
let path = check(VolumeRuns::default()).discovery(DONE_HOLDS).expect("found");
// For a resize property, set the scenario: VolumeRuns { scenario: Scenario::Resize, ..VolumeRuns::default() }
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
3. If a property's meaning changes, change its text and say why in the PR body. Narrow a property only when the code is right and the property was wrong about the design. Before narrowing, recheck the model step the counterexample runs through: a forward Release once failed "Release wins" only because the model gave it a fresh epoch where `endAndRelease` hands it the Move's.

## Bounds and blind spots

Bounds: Machines A and B, one resize in one direction, between two sizes, before the Move, one Move and one takeover, one resend per run, one daemon crash, one Busy retry. The resize resends only in the `Resize` scenario. One Machine at most can't take the target, and a shrink floor never moves. A bug that needs more of any of these is invisible.

Out of scope: Deploy lanes, operator collisions, Machines that never answer, warm rounds beyond the one pull that brings in a B the resize never asked, a second takeover, real Pool capacities and floors, ZFS, the Store's record transaction, a second resize, a settle (a second resize at the recorded size), a resize during or after a Move, Restore, and anything inside `plan`'s callers that the model does not copy. A green model says nothing about them; cover them with tests or `verify-server`.
