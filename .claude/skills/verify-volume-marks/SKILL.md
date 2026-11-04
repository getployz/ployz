---
name: verify-volume-marks
description: Model-check a Volume.Seed request against the ZFS ployz:* marks (seed, promote, cut, pulled) on one Machine, while Docker's Create adopts the seeded root, a later Move cuts it away, and a mirror pull brings it back as a Mirror, with the Stateright model in core/crates/ployz-verify. Use when a change touches core/crates/ployzd/src/volume_plugin/seed.rs (seed_volume, land, finish_landing), copy.rs (copy_cut, copy_pull, mark_pulled), departure.rs (written_here, marked_written_here), storage.rs (create_volume's adoption, admit_mutation), transfer.rs (Receives::running), the Volume.Seed route in machine_api/local.rs or admit_gen in machine_api/local_volume_copy.rs, SeedVolumeRequest in ployz-core's volume_switch.rs, or Cloud's seedVolume and sendSwitch in dashboard/src/modules/volume-run/.
---

# Verify Volume marks

The model in `core/crates/ployz-verify/src/volume_marks.rs` explores every interleaving of one Cloud `seed` run with the Machine A that holds the restored Volume `into`. Cloud's `seedVolume` resends `seed_volume` until A answers a root (`dashboard/src/modules/volume-run/volume-run.inngest.ts:761-790`). A transport failure fails only the attempt, so the step resends while the first request is still in transit (`volume-run.server.ts:1194`, `1204`). On A, `VolumeStorage::seed_volume` copies the restore point into a slot and lands it as a root (`core/crates/ployzd/src/volume_plugin/seed.rs:27-127`). Docker's Create adopts it by inheriting `ployz:promote` (`storage.rs:200-207`). A later Move cuts it into an old copy (`copy.rs:245-323`). The later run's Mirror want pulls the new writer's snapshot back into that slot (`copy.rs:566-740`), and `mark_pulled` re-marks the slot (`copy.rs:799-821`). Nothing calls the real daemon. The model is a hand copy, and it drifts unless you keep it in step.

Each A handler is one model step, because every plugin handler holds `admit_mutation` from its first read to its answer (`storage.rs:178-187`, `seed.rs:31`, `copy.rs:148`, `pool.rs:103` for Create). A receive task runs outside that lock (`seed.rs:123-125`, `copy.rs:722-734`). So a receive completing and `mark_pulled` are separate steps, and `Receives::running` reads true until the task ends (`transfer.rs:124-132`).

The properties, each one sentence:

- `STALE_KEEPS_LATER`: **a Seed from an ended seed run never destroys a copy of `into` a later run made.** This is the finding. The harm: the Mirror the later run pulled is silently reset to the old restore point, so A no longer holds a current copy of `into`.
- `KEEPS_ALONE`: **a Seed never destroys a copy of `into` only this Machine may hold.** This is what `written_here` promises (`departure.rs:230-246`).
- `KEEPS_RECEIVING`: **a Seed never destroys a slot a receive is still writing.**
- `DONE_HOLDS_POINT`: **a seed run ends done only on a root holding its restore point.** Cloud ends the run done on any root answer (`volume-run.inngest.ts:783`, `788`).
- `EVERY_ROW_ENDS`: **at rest, the seed run's row has ended.**

Each property is phrased over a stale Seed, or over a copy no writer elsewhere holds, because a live Seed replacing a pulled Mirror is designed. `a_seed_replaces_a_slot_a_mirror_pull_marked_or_that_holds_no_point` pins that design (`core/crates/ployzd/src/volume_plugin/tests/seed_tests.rs:595-631`).

## Run it

```
cd core && cargo test -p ployz-verify volume_marks -- --nocapture --test-threads=1
```

A warm run of the eight tests takes about a second, since the largest scenario has 238 states. The whole crate took 111 s at the sync commit on a shared host, almost all of it in the other models. With one test thread, each story prints in order under its test's name, which appears in brackets above it. CI runs it in the workspace nextest, so a red model blocks the PR.

The run is green when all eight tests pass. Read them as three groups.

- **The finding.** `current_code_lets_a_late_seed_destroy_a_mirror_a_later_run_pulled` passes by finding the counterexample on the code as it is, and prints it. `current_code_breaks_only_the_late_seed_property` checks the code as it is in every scenario, and expects every other property to hold and the finding to need the later Move (`Scenario::Fresh`). When the product fix lands, model the fix as the code writes it. Make the fixed model the default, and turn the finding test into a `Broken` flag that restores the old Seed, as `removal_marks.rs` `finish_needs_its_conditional_save` did.
- **The fix variant.** `the_floor_keeps_a_late_seed_off_a_later_copy` runs the fix (`fix: true`) in every scenario. Seed carries its run's gen, and `admit_gen` admits it against `into`'s floor under the switch lock, as `set_copy_admitted` checks `SetVolumeCopy` again under the lock (`core/crates/ployzd/src/machine_api/local_volume_copy.rs:86-88`, `258-268`; `docker/spec_store.rs:525-557`). The fix needs that check under the lock, and the model's one atomic Seed step models only that one. A check before the lock, as `set_copy` also makes (`local_volume_copy.rs:47`), is optional. A check made only before the lock fails: PR #3836's round-1 replay admitted a Seed at floor 2, let Adopt, Cut, Commit, Pull, Received and MarkPulled raise the floor to 3 while it waited for the lock, and the admitted Seed then destroyed the Mirror the later run pulled. Splitting the step to show that is follow-up E-21b. Every property holds, and every `sometimes` is reached. Each `sometimes` is one `COVERAGE` row naming a branch the code handles. `every_sometimes_needs_what_it_names` takes each row's `omit` out of the fixed model and expects the row unreached. A row reached without its cut has a predicate looser than its name, and that test stays red until the predicate names the branch.
- **The guards.** Four tests each drop one guard the code has (a `Broken` flag) and expect the property it keeps to fail. `a_root_needs_its_seed_guid_checked` drops the `ployz:seed` refusal on a root (`seed.rs:43-46`) and breaks `DONE_HOLDS_POINT`. `a_seed_needs_to_wait_for_a_running_receive` drops `receives.running` (`seed.rs:55-57`) and breaks `KEEPS_RECEIVING`. `a_seed_needs_written_here` drops the `written_here` refusal (`seed.rs:97-103`) and breaks `KEEPS_ALONE`. `a_live_seed_replaces_a_pulled_mirror_once_mark_pulled_clears_its_marks` drops the mark clearing in `mark_pulled` (`copy.rs:807-816`) and leaves `a live Seed replaces a Mirror a pull received` unreached. If one of these goes from green to red, the guard no longer earns its keep, or the model lost the path that shows it.

## Read a counterexample

A failure prints the test name, the property, the model's flags, and a numbered story cut at the first state in BFS order that breaks the property, so the last row is the broken state. `breaks` keeps that first path with a visitor. Each row is one step in the code's words, followed by the row, the wire, A's floor and `into`'s datasets once it ran, for example:

```
14. A: Volume.Seed runs a request, its row ENDED; under the switch lock and admit_mutation, written_here(fs) reads false: zfs destroy -r the slot parent, a new slot, copy_snapshot of name@G (seed.rs seed_volume); no step waits for the answer
    row=Done(Seed) on_wire=0 floor=3 | into: slot, no fs | task=Copying
```

`row` is the seed run's row. It is `Open { awaiting }` while the step `03-ask-{poll}` waits, `Done(made)` with whose data the answered root held, or `Failed`. `on_wire` counts Seed requests sent and not yet run by A. All of them carry `{name, into, guid: G}` (`core/crates/ployz-core/src/volume_switch.rs:279-283`). `floor` is A's `volume_floors` row for `into`. Gens are `EARLIER = 1`, `SEED = 2` and `LATER = 3`, since every run takes its gen from one identity column (`dashboard/src/modules/volume-run/tables.ts:55`). `into` is absent, a root, or a slot with or without its `fs`. Each copy shows its newest snapshot, its local `ployz:pulled`, the marks `written_here` reads, and `made`, the run whose data it holds. `task` is the receive task `Receives` keeps for `into`. A Seed row names the branch of `seed_volume` it took, and says `(under Broken)` where a flag skipped a guard.

Write the story out in the code's words before deciding anything: which run's request landed, whether its row had ended, and what `mark_pulled` left on the slot by then. A test that fails inside `assert_properties` prints the raw path of actions instead. To see the rows, call the module's `breaks` helper with that model in a scratch test.

If a picture would help, the pstack `/statechart` skill draws how a model moves between states and tells each shortest failing path in plain words. Refer to it by name, since its commands change between releases.

## Real bug or model error

A counterexample is a **lead**, not a verdict. Confirm each step of the story against the function it names before calling it a bug. Each `Action` and `SeedPath` variant's doc comment names its file and function, and `reads_as` prints the statement each step copies.

- If a step does something the code cannot, fix the model and say so. Three places are the usual suspects. `actions` decides when the later run's steps may run. `seed` holds the order of `seed_volume`'s branches (`seed.rs:38-126`). `Received` and `MarkPulled` decide what a pull leaves on the slot.
- If every step matches the code, it is a finding. Report it in plain words with the path. Keep the property and the model as they are, because weakening either to go green hides the bug.

The finding this model was built on, confirmed step by step at the sync commit below, has a path of 15 rows.

1. The seed run sends Seed, and the attempt fails short of an answer, so the request stays in transit while the step resends (`volume-run.server.ts:1194`).
2. The resend finds no slot, makes one, and starts `copy_snapshot` (`seed.rs:109-125`). Another resend, after the copy completes, lands it (`seed.rs:65-71`, `141-161`). The root carries `ployz:seed` and `ployz:promote` (`seed.rs:153-158`). Cloud reads the root and ends the run done (`volume-run.inngest.ts:783`, `788`).
3. Docker's Create adopts the root by inheriting only `ployz:promote` (`storage.rs:202-204`). `ployz:seed` stays.
4. The later Move's WriterToMirror admits its gen, so A's floor for `into` is 3 (`local_volume_copy.rs:47`). It sets `ployz:cut` (`copy.rs:291`) and renames the root into the slot's `fs` (`copy.rs:320`). At this point `written_here` reads true.
5. The Move commits on B, and B's writer takes a newer snapshot. The later run's Mirror want on A starts an incremental receive into the slot (`copy.rs:684-734`).
6. The receive exits 0, and `mark_pulled` clears (`zfs inherit`) `ployz:cut`, `epoch`, `promote` and `seed` (`copy.rs:807-816`), then sets `ployz:pulled` to the newest guid (`copy.rs:817`).
7. The stranded request from step 1 lands. Its row ended long ago. Volume.Seed takes only the switch lock and no floor (`machine_api/local.rs:227-235`, `557-563`). The root is gone, no receive runs, and the slot's newest snapshot is not `G`. `written_here` reads false, since no mark is set and `ployz:pulled` names the newest (`departure.rs:238-246`). So `seed_volume` runs `zfs destroy -r` on the slot parent and starts copying `G` into it (`seed.rs:104-125`).

What the model assumes, so you can judge the lead. The request waits in transit before the switch lock, across the Deploy, a Move and a finished pull. `switch_mutation` is a `tokio::sync::Mutex` (`machine_api/local.rs:43`), which serves waiters first in, first out, and the cut takes the same lock (`local_volume_copy.rs:50-52`). So a request already queued at the lock cannot by itself arrive this late. The delay has to come before A takes the request. No test or Boat run on main shows the path yet. The model also keeps `name`'s writer root on A with `G` throughout, which `seed.rs:73-92` needs.

## A break that stays green

Guards overlap, so removing one can leave every property holding. Before calling a guard dead, rerun with a neighbouring `Broken` flag set too, or with the other value of `fix`.

- Under the fix, the floor refuses a Seed whose gen is below a floor a later run raised, before any `seed.rs` guard runs. Ending a row raises no floor, so a request from an ended row still passes admission while the floor equals its gen, and then meets the guards: it answers the root it landed. So each `seed.rs` guard shows its worth on a Seed the floor admits, and none of the guard tests needs a later run. That is why each guard test runs in the scenario that gives a live Seed something to hit. `Scenario::Taken` starts with another Volume holding the root. `Scenario::OldCopy` starts with a cut-marked old copy an earlier Volume named `into` left. `Scenario::Fresh` has the seed's own copy receiving.
- The mark clearing in `mark_pulled` is a liveness guard, not a safety guard. With the marks kept, `written_here` reads true on every pulled Mirror, so no Seed ever replaces one. On the code as it is, that also refuses the stranded request, so `STALE_KEEPS_LATER` holds. The test asserts both. The clearing is what opens the finding's path, and the fix is the floor, not a stricter `written_here`, since a stricter one would break the design the seed tests pin.
- The fix's floor is raised by the Seed itself. In `Scenario::OldCopy` nothing else raises it above the earlier run's gen, so under the fix an earlier run's `SetVolumeCopy` on `into` that arrives after a Seed is refused (`admit_gen refuses the earlier run's pull below into's floor`). That is a behaviour change the product fix must argue.

## Keep the model in step

When a change intentionally alters the Seed or the marks (a new guard, a new branch, a reordered check, a new mark, a new caller of `written_here`), change the model in the same PR.

1. Update the matching `Action`, `SeedPath`, or the arm of `seed` or `step`, citing the file and function it copies, and the `reads_as` line that prints it.
2. If the change adds a guard, add a `Broken` flag that removes it and a `breaks` test naming the property it keeps. A guard with no failing variant is untested.
3. If the change ships the fix, model it as the code writes it and flip the finding test as described under Run it. Do not delete it, since its printed path is the record of what the fix keeps out.
4. If a property's meaning changes, change its text and say why in the PR body. Narrow a property only when the code is right and the property was wrong about the design.
5. If the change adds a branch, add a `COVERAGE` row naming it, with the `omit` that takes it out.
6. Move the sync commit below forward only after you confirmed each covered function still reads as the model says.

## Bounds and blind spots

These are the bounds. There is one Machine A, one seed run that sends at most three requests (`MAX_SENDS`), and at most one request that outlives its attempt (`MAX_TIMEOUTS`). The later run is one Deploy and one Move, with one Mirror want back on A. The earlier run in `Scenario::OldCopy` sends one Mirror want. B is not modelled beyond the Move committing. Every A handler is one atomic step under `admit_mutation`.

These are out of scope:

- a seed copy that fails, and the `failure` branch that answers it once (`seed.rs:58-64`)
- `ployz:epoch`, since no modelled copy carries it
- a departure snapshot, and the departure itself, which also destroys on `written_here`
- `ployz:pull` and `ployz:role` on the slot parent
- a crash between the `zfs` calls of one handler, so `finish_landing` never reruns
- `name`'s writer losing `G` (`RestorePointGone`, `seed.rs:82-92`)
- a full pull resend, which destroys `fs` inside the task (`copy.rs:784-787`)
- Busy answers (`volume-run.inngest.ts:781`)
- two stranded requests, where the second could land a root of `into` on A while B is the writer. The model has not checked this.

A green model says nothing about them. Cover them with the seed tests in `core/crates/ployzd/src/volume_plugin/tests/seed_tests.rs` (`cargo nextest run -p ployzd volume_plugin::`).

## Coverage

Each model step is a copy of code at the sync commit. Reread these when a diff touches them:

- `core/crates/ployzd/src/volume_plugin/seed.rs`: `seed_volume` whole, with the root branch and its guid refusal, `receives.running`, `failure`, the landing on a matching newest, the source checks, the `written_here` refusal, `destroy -r`, `create_bounded_slot` and `receives.spawn`. Also `land` and `finish_landing`.
- `core/crates/ployzd/src/volume_plugin/copy.rs`: `admit`, `copy_cut` with its `ployz:cut` mark and rename, `copy_pull` with its `receives.running` check and the spawned task, `pull_stream`, and `mark_pulled` with its inherit loop and its `ployz:pulled` set.
- `core/crates/ployzd/src/volume_plugin/departure.rs`: `written_here` and `marked_written_here`.
- `core/crates/ployzd/src/volume_plugin/storage.rs`: `admit_mutation`, the adoption in `create_volume`, and the mark constants.
- `core/crates/ployzd/src/volume_plugin/transfer.rs`: `Receives::running` and `spawn`.
- `core/crates/ployzd/src/machine_api/local.rs`: `switch_mutation`, `switch_verb` and `seed_volume`.
- `core/crates/ployzd/src/machine_api/local_volume_copy.rs`: `set_copy` with `admit_gen` before the lock, `set_copy_admitted` with `admit_gen` again under the lock, and `admit_gen`.
- `core/crates/ployzd/src/docker/spec_store.rs`: `FloorTurn::admit`.
- `core/crates/ployz-core/src/volume_switch.rs`: `SeedVolumeRequest`.
- `dashboard/src/modules/volume-run/volume-run.inngest.ts`: `seedVolume`.
- `dashboard/src/modules/volume-run/volume-run.server.ts`: `sendSwitch`.
- `dashboard/src/modules/volume-run/tables.ts`: the `gen` identity column.
- `core/crates/ployzd/src/volume_plugin/tests/seed_tests.rs`: `a_seed_replaces_a_slot_a_mirror_pull_marked_or_that_holds_no_point`, the designed replacement the fix variant must keep.

Sync commit: `0928c565975b97f0446d60298f30a3a5267f1a97`, the `main` this model was checked against, and every line cited above is at it. Code in the list above that changed after it has not been rechecked.
