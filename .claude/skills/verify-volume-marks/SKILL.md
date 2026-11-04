---
name: verify-volume-marks
description: Model-check a Volume.Seed request against the ZFS ployz:* marks (seed, promote, cut, pulled) on one Machine, while Docker's Create adopts the seeded root, a later Move cuts it away, and a mirror pull brings it back as a Mirror, with the Stateright model in core/crates/ployz-verify. Use when a change touches core/crates/ployzd/src/volume_plugin/seed.rs (seed_volume, land, finish_landing), copy.rs (copy_cut, copy_pull, mark_pulled), departure.rs (written_here, marked_written_here), storage.rs (create_volume's adoption, admit_mutation), transfer.rs (Receives::running), the Volume.Seed route in machine_api/local.rs, admit_gen or check_gen in machine_api/local_volume_copy.rs, FloorTurn in docker/spec_store.rs, SeedVolumeRequest in ployz-core's volume_switch.rs, or Cloud's seedVolume and sendSwitch in dashboard/src/modules/volume-run/.
---

# Verify Volume marks

The model in `core/crates/ployz-verify/src/volume_marks.rs` explores every interleaving of one Cloud `seed` run with the Machine A that holds the restored Volume `into`. Cloud's `seedVolume` resends `seed_volume` until A answers a root (`dashboard/src/modules/volume-run/volume-run.inngest.ts:761-790`). A transport failure fails only the attempt, so the step resends while the first request is still in transit (`volume-run.server.ts:1194`, `1204`). On A, `VolumeStorage::seed_volume` copies the restore point into a slot and lands it as a root (`core/crates/ployzd/src/volume_plugin/seed.rs:27-127`). Docker's Create adopts it by inheriting `ployz:promote` (`storage.rs:200-207`). A later Move cuts it into an old copy (`copy.rs:245-323`). The later run's Mirror want pulls the new writer's snapshot back into that slot (`copy.rs:566-740`), and `mark_pulled` re-marks the slot (`copy.rs:799-821`). Nothing calls the real daemon. The model is a hand copy, and it drifts unless you keep it in step. `ployz-verify` dev-depends only on `ployz-core` and `stateright` (`core/crates/ployz-verify/Cargo.toml`), not on `ployzd`, so a product change leaves the model green, without even recompiling it, until you copy the change in.

Each A handler is one model step, because every plugin handler holds `admit_mutation` from its first read to its answer (`storage.rs:178-187`, `seed.rs:31`, `copy.rs:148`, `pool.rs:103` for Create). A receive task runs outside that lock (`seed.rs:123-125`, `copy.rs:722-734`). So a receive completing and `mark_pulled` are separate steps, and `Receives::running` reads true until the task ends (`transfer.rs:124-132`).

The properties, each one sentence:

- `STALE_KEEPS_LATER`: **a Seed from an ended seed run never destroys a copy of `into` a later run made.** This was the finding, which the floor check in `seed_volume` fixes (`core/crates/ployzd/src/machine_api/local.rs:562-570`). The harm: the Mirror the later run pulled is silently reset to the old restore point, so A no longer holds a current copy of `into`.
- `KEEPS_ALONE`: **a Seed never destroys a copy of `into` only this Machine may hold.** This is what `written_here` promises (`departure.rs:230-246`).
- `KEEPS_RECEIVING`: **a Seed never destroys a slot a receive is still writing.**
- `DONE_HOLDS_POINT`: **a seed run ends done only on a root holding its restore point.** Cloud ends the run done on any root answer (`volume-run.inngest.ts:783`, `788`).
- `EVERY_ROW_ENDS`: **at rest, the seed run's row has ended.**

Each property is phrased over a stale Seed, or over a copy no writer elsewhere holds, because a live Seed replacing a pulled Mirror is designed. `a_seed_replaces_a_slot_a_mirror_pull_marked_or_that_holds_no_point` pins that design (`core/crates/ployzd/src/volume_plugin/tests/seed_tests.rs:595-631`).

## Run it

```
cd core && cargo test -p ployz-verify volume_marks -- --nocapture --test-threads=1
```

A warm run of the ten tests takes about a second, since the largest scenario has 238 states. The whole crate took 111 s at `0928c565` on a shared host, almost all of it in the other models. With one test thread, each story prints in order under its test's name, which appears in brackets above it. CI runs it in the workspace nextest, so a red model blocks the PR.

The run is green when all ten tests pass. Read them as three groups.

- **The holds test.** `the_floor_keeps_a_late_seed_off_a_later_copy` runs the code as it is in every scenario. Seed carries its run's gen (`core/crates/ployz-core/src/volume_switch.rs:285-289`), and Cloud sends the seed run's (`dashboard/src/modules/volume-run/volume-run.inngest.ts:778`). `seed_volume` takes the switch lock and then checks that gen against `into`'s floor, before the plugin's `seed_volume` runs (`core/crates/ployzd/src/machine_api/local.rs:562-571`). The check is `check_gen` (`local_volume_copy.rs:273-283`) through `FloorTurn::check` (`docker/spec_store.rs:552-565`), which refuses a gen below the floor and writes nothing, so an equal gen passes and the floor stays where it was. The model's one atomic Seed step is that check and the plugin call together. Every property holds, and every `sometimes` is reached. Each `sometimes` is one `COVERAGE` row naming a branch the code handles. `every_sometimes_needs_what_it_names` takes each row's `omit` out of the model and expects the row unreached. A row reached without its cut has a predicate looser than its name, and that test stays red until the predicate names the branch.
- **The finding, kept as a guard.** `a_seed_needs_the_floor_of_into` sets `Broken::seed_skips_floor`, the Seed before the fix, and prints the path that breaks `STALE_KEEPS_LATER`. The same Seed runs today when the request has no gen, since `seed_volume` checks only `Some` (`local.rs:566-567`). That covers a Cloud that sends none, and a Machine that predates the field, which drops it as an unknown field (`SeedVolumeRequest` has no `deny_unknown_fields`, `volume_switch.rs:280-290`). `without_the_floor_only_the_late_seed_property_breaks` expects every other property to hold under that flag in every scenario, and the finding to need the later Move (`Scenario::Fresh`).
- **The guards.** Six more tests each drop one guard (a `Broken` flag) and expect what it keeps to fail. `a_seed_needs_the_floor_checked_under_the_switch_lock` (`floor_checked_only_before_lock`) checks the floor before the lock instead of under it, and breaks `STALE_KEEPS_LATER`: the stranded request passes the check at floor 0, then Adopt, Cut, Commit, Pull, Received and MarkPulled run before it takes the lock, and it destroys the Mirror the later run pulled, as PR #3836's round-1 replay found. `a_seed_leaves_the_floor_of_into_where_it_found_it` (`seed_raises_floor`) makes the Seed's check raise the floor, as `FloorTurn::admit` does (`docker/spec_store.rs:538-543`), and leaves `admit_gen admits the earlier run's pull after a Seed of into ran` unreached. `a_root_needs_its_seed_guid_checked` drops the `ployz:seed` refusal on a root (`seed.rs:43-46`) and breaks `DONE_HOLDS_POINT`. `a_seed_needs_to_wait_for_a_running_receive` drops `receives.running` (`seed.rs:55-57`) and breaks `KEEPS_RECEIVING`. `a_seed_needs_written_here` drops the `written_here` refusal (`seed.rs:97-103`) and breaks `KEEPS_ALONE`. `a_live_seed_replaces_a_pulled_mirror_once_mark_pulled_clears_its_marks` drops the mark clearing in `mark_pulled` (`copy.rs:807-816`) and leaves `a live Seed replaces a Mirror a pull received` unreached. If one of these goes from green to red, the guard no longer earns its keep, or the model lost the path that shows it.

Why the Seed's check must not raise the floor: a mirror sync of `into` carries the newest gen among `into`'s own runs, 0 when it has none (`dashboard/src/modules/volume-placement/mirror-sync.server.ts:174-179`, `229`). The seed run is not one of them, because its lane is the source Volume's (`volume-run.server.ts:566`). So until `into` has a run of its own, a floor the seed run had raised would refuse every mirror sync of `into` Stale. The model has no mirror sync; the earlier run's pull, at a gen below the seed run's, stands in for it.

## Read a counterexample

A failure prints the test name, the property, the model's flags, and a numbered story cut at the first state in BFS order that breaks the property, so the last row is the broken state. `breaks` keeps that first path with a visitor. Each row is one step in the code's words, followed by the row, the wire, A's floor and `into`'s datasets once it ran, for example:

```
14. A: Volume.Seed runs a request, its row ENDED; under the switch lock, checking no floor (under Broken), and admit_mutation, written_here(fs) reads false: zfs destroy -r the slot parent, a new slot, copy_snapshot of name@G (seed.rs seed_volume); no step waits for the answer
    row=Done(Seed) on_wire=0 queued=0 floor=3 | into: slot, no fs | task=Copying
```

`row` is the seed run's row. It is `Open { awaiting }` while the step `03-ask-{poll}` waits, `Done(made)` with whose data the answered root held, or `Failed`. `on_wire` counts Seed requests sent and not yet run by A. All of them carry `{name, into, guid: G, gen: SEED}` (`core/crates/ployz-core/src/volume_switch.rs:281-290`). `queued` is nonzero only under `floor_checked_only_before_lock`: requests past the floor check that have not taken the switch lock yet. `floor` is A's `volume_floors` row for `into`. Gens are `EARLIER = 1`, `SEED = 2` and `LATER = 3`, since every run takes its gen from one identity column (`dashboard/src/modules/volume-run/tables.ts:55`). `into` is absent, a root, or a slot with or without its `fs`. Each copy shows its newest snapshot, its local `ployz:pulled`, the marks `written_here` reads, and `made`, the run whose data it holds. `task` is the receive task `Receives` keeps for `into`. A Seed row names the branch of `seed_volume` it took, and says `(under Broken)` where a flag skipped a guard.

Write the story out in the code's words before deciding anything: which run's request landed, whether its row had ended, and what `mark_pulled` left on the slot by then. A test that fails inside `assert_properties` prints the raw path of actions instead. To see the rows, call the module's `breaks` helper with that model in a scratch test inside `volume_marks::tests`, since `breaks` is private there.

If a picture would help, the pstack `/statechart` skill draws how a model moves between states and tells each shortest failing path in plain words. Refer to it by name, since its commands change between releases.

## Real bug or model error

A counterexample is a **lead**, not a verdict. Confirm each step of the story against the function it names before calling it a bug. Each `Action` and `SeedPath` variant's doc comment names its file and function, and `reads_as` prints the statement each step copies.

- If a step does something the code cannot, fix the model and say so. Three places are the usual suspects. `actions` decides when the later run's steps may run. `seed` holds the order of `seed_volume`'s branches (`seed.rs:38-126`). `Received` and `MarkPulled` decide what a pull leaves on the slot.
- If every step matches the code, it is a finding. Report it in plain words with the path. Keep the property and the model as they are, because weakening either to go green hides the bug.

The finding this model was built on, confirmed step by step at `0928c565975b97f0446d60298f30a3a5267f1a97`, before the fix, has a path of 15 rows. Step 7's citations of `machine_api/local.rs` hold at that commit, not at the sync commit below.

1. The seed run sends Seed, and the attempt fails short of an answer, so the request stays in transit while the step resends (`volume-run.server.ts:1194`).
2. The resend finds no slot, makes one, and starts `copy_snapshot` (`seed.rs:109-125`). Another resend, after the copy completes, lands it (`seed.rs:65-71`, `141-161`). The root carries `ployz:seed` and `ployz:promote` (`seed.rs:153-158`). Cloud reads the root and ends the run done (`volume-run.inngest.ts:783`, `788`).
3. Docker's Create adopts the root by inheriting only `ployz:promote` (`storage.rs:202-204`). `ployz:seed` stays.
4. The later Move's WriterToMirror admits its gen, so A's floor for `into` is 3 (`local_volume_copy.rs:47`). It sets `ployz:cut` (`copy.rs:291`) and renames the root into the slot's `fs` (`copy.rs:320`). At this point `written_here` reads true.
5. The Move commits on B, and B's writer takes a newer snapshot. The later run's Mirror want on A starts an incremental receive into the slot (`copy.rs:684-734`).
6. The receive exits 0, and `mark_pulled` clears (`zfs inherit`) `ployz:cut`, `epoch`, `promote` and `seed` (`copy.rs:807-816`), then sets `ployz:pulled` to the newest guid (`copy.rs:817`).
7. The stranded request from step 1 lands. Its row ended long ago. Volume.Seed takes only the switch lock and no floor (`machine_api/local.rs:227-235`, `557-563`). The root is gone, no receive runs, and the slot's newest snapshot is not `G`. `written_here` reads false, since no mark is set and `ployz:pulled` names the newest (`departure.rs:238-246`). So `seed_volume` runs `zfs destroy -r` on the slot parent and starts copying `G` into it (`seed.rs:104-125`).

What the model assumes, so you can judge the lead. The request waits in transit before the switch lock, across the Deploy, a Move and a finished pull. `switch_mutation` is a `tokio::sync::Mutex` (`machine_api/local.rs:43`), which serves waiters first in, first out, and the cut takes the same lock (`local_volume_copy.rs:50-52`). So a request already queued at the lock cannot by itself arrive this late. The delay has to come before A takes the request. Under `floor_checked_only_before_lock`, `queued` stands for that delay between the check and the lock, not for a place in the lock's queue. `a_seed_below_the_floor_of_into_refuses_stale_before_any_effect` (`core/crates/ployzd/src/machine_api/local_tests.rs`) is red on main `d44c048596`: a Seed whose gen is below `into`'s floor reaches the plugin. The model also keeps `name`'s writer root on A with `G` throughout, which `seed.rs:73-92` needs.

## A break that stays green

Guards overlap, so removing one can leave every property holding. Before calling a guard dead, rerun with a neighbouring `Broken` flag set too, or with the other value of `fix`.

- The floor refuses a Seed whose gen is below a floor a later run raised, before any `seed.rs` guard runs (`machine_api/local.rs:566-571`). Ending a row raises no floor, and neither does the Seed's check (`docker/spec_store.rs:552-565`). At an equal floor, an ended row's request still reaches the `seed.rs` guards, since the check refuses only `gen < floor` (`spec_store.rs:561`). If the run landed, it answers that root (`seed.rs:38-53`). If its copy is still receiving, it answers the copy (`seed.rs:55-57`). So each `seed.rs` guard shows its worth on a Seed the floor admits, and none of the guard tests needs a later run. That is why each guard test runs in the scenario that gives a live Seed something to hit. `Scenario::Taken` starts with another Volume holding the root. `Scenario::OldCopy` starts with a cut-marked old copy an earlier Volume named `into` left. `Scenario::Fresh` has the seed's own copy receiving.
- The mark clearing in `mark_pulled` is a liveness guard, not a safety guard. With the marks kept, `written_here` reads true on every pulled Mirror, so no Seed ever replaces one. Under `seed_skips_floor`, that also refuses the stranded request, so `STALE_KEEPS_LATER` holds. The test asserts both. The clearing is what opens the finding's path, and the fix is the floor, not a stricter `written_here`, since a stricter one would break the design the seed tests pin.
- The two halves of `written_here` overlap within the model's bounds. Dropping the `ployz:pulled` comparison (`departure.rs:242-245`) keeps all ten tests green. The cases where that comparison alone keeps a slot are outside the model: a crash partway through `mark_pulled`, after its marks are cleared and before `ployz:pulled` is set (`copy.rs:807-820`), and a slot from a release before the mark. Ignoring the seed, promote, cut and epoch marks (`departure.rs:239-241`, `250-272`) fails only `a_live_seed_replaces_a_pulled_mirror_once_mark_pulled_clears_its_marks`, a liveness check. No safety property catches it, because with `pull_keeps_marks` off (the code as modelled) every slot that carries a mark has no `ployz:pulled`, so the comparison alone already keeps it.
- A Seed whose check raised the floor (`seed_raises_floor`) breaks no safety property in any scenario. Only a request whose gen is below the seed run's can tell, and the model has one: the earlier run's pull in `Scenario::OldCopy`. So the raise shows only as `admit_gen admits the earlier run's pull after a Seed of into ran` left unreached, which is what `a_seed_leaves_the_floor_of_into_where_it_found_it` asserts. The harm it stands for, a refused mirror sync, is outside the model.

## Keep the model in step

When a change intentionally alters the Seed or the marks (a new guard, a new branch, a reordered check, a new mark, a new caller of `written_here`), change the model in the same PR.

1. Update the model function that copies the product function you changed, citing the file and function it copies, and the `reads_as` line that prints it:
   - `seed_volume` (`seed.rs`): `VolumeMarks::seed`, its `SeedPath` variants, and `reads_as`'s Seed arm.
   - `written_here` and `marked_written_here` (`departure.rs`): `VolumeMarks::written_here` and `Marks::any`.
   - `admit_gen` (`machine_api/local_volume_copy.rs`) and `FloorTurn::admit` (`docker/spec_store.rs`): `VolumeMarks::admit`.
   - `seed_volume` (`machine_api/local.rs`), `check_gen` (`machine_api/local_volume_copy.rs`) and `FloorTurn::check` (`docker/spec_store.rs`): `VolumeMarks::check` and the floor check at the top of `VolumeMarks::seed`, and `Admit` under `floor_checked_only_before_lock`.
   - `seedVolume` reading an answer (`volume-run.inngest.ts`): `VolumeMarks::answer`. Its resend loop and `sendSwitch`'s attempt failures (`volume-run.server.ts`): the `Send`, `TimeOut` and `GiveUp` offers in `actions`, with `MAX_SENDS` and `MAX_TIMEOUTS`.
   - `copy_cut`, `copy_pull`, `pull_stream` and `mark_pulled` (`copy.rs`), `create_volume`'s adoption (`storage.rs`), and `copy_snapshot` completing (`seed.rs`): the `Cut`, `Pull`, `EarlierPull`, `Received`, `MarkPulled`, `Adopt` and `CopyDone` arms of `step`, and when `actions` offers each, with `slot_holds`.
   - `Receives::running` (`transfer.rs`): `Task`, and the `Receiving` arm of `VolumeMarks::seed`.
2. If the change adds a guard, add a `Broken` flag that removes it and a `breaks` test naming the property it keeps. A guard with no failing variant is untested.
3. Keep `a_seed_needs_the_floor_of_into`. Its printed path is the record of what the floor keeps out, and the path a Seed without a gen still takes.
4. If a property's meaning changes, change its text and say why in the PR body. Narrow a property only when the code is right and the property was wrong about the design.
5. If the change adds a branch, add a `COVERAGE` row naming it, with the `omit` that takes it out.
6. Move the sync commit below forward only after you confirmed each covered function still reads as the model says.

## Bounds and blind spots

These are the bounds. There is one Machine A, one seed run that sends at most three requests (`MAX_SENDS`), all carrying its gen, and at most one request that outlives its attempt (`MAX_TIMEOUTS`). The later run is one Deploy and one Move, with one Mirror want back on A. The earlier run in `Scenario::OldCopy` sends one Mirror want. B is not modelled beyond the Move committing. Every A handler is one atomic step under `admit_mutation`.

These are out of scope:

- a seed copy that fails, and the `failure` branch that answers it once (`seed.rs:58-64`)
- `ployz:epoch`, since no modelled copy carries it
- a departure snapshot, and the departure itself, which also destroys on `written_here`
- `ployz:pull` and `ployz:role` on the slot parent
- a crash between the `zfs` calls of one handler, so `finish_landing` never reruns
- `name`'s writer losing `G` (`RestorePointGone`, `seed.rs:82-92`)
- a full pull resend, which destroys `fs` inside the task (`copy.rs:784-787`)
- Busy answers (`volume-run.inngest.ts:781`)
- a crash partway through `mark_pulled`, which leaves a slot with its marks cleared and no `ployz:pulled`
- a slot from a release before `ployz:pulled`, which only the pulled comparison in `written_here` keeps
- two stranded requests, where the second could land a root of `into` on A while B is the writer. The model has not checked this.

A green model says nothing about them. Cover them with the seed tests in `core/crates/ployzd/src/volume_plugin/tests/seed_tests.rs` (`cargo nextest run -p ployzd volume_plugin::`).

## Coverage

Each model step is a copy of code at the sync commit. Reread these when a diff touches them:

- `core/crates/ployzd/src/volume_plugin/seed.rs`: `seed_volume` whole, with the root branch and its guid refusal, `receives.running`, `failure`, the landing on a matching newest, the source checks, the `written_here` refusal, `destroy -r`, `create_bounded_slot` and `receives.spawn`. Also `land` and `finish_landing`.
- `core/crates/ployzd/src/volume_plugin/copy.rs`: `admit`, `copy_cut` with its `ployz:cut` mark and rename, `copy_pull` with its `receives.running` check and the spawned task, `pull_stream`, and `mark_pulled` with its inherit loop and its `ployz:pulled` set.
- `core/crates/ployzd/src/volume_plugin/departure.rs`: `written_here` and `marked_written_here`.
- `core/crates/ployzd/src/volume_plugin/storage.rs`: `admit_mutation`, the adoption in `create_volume`, and the mark constants.
- `core/crates/ployzd/src/volume_plugin/transfer.rs`: `Receives::running` and `spawn`.
- `core/crates/ployzd/src/machine_api/local.rs`: `switch_mutation` and `seed_volume` with its floor check under the lock.
- `core/crates/ployzd/src/machine_api/local_volume_copy.rs`: `set_copy` with `admit_gen` before the lock, `set_copy_admitted` with `admit_gen` again under the lock, `admit_gen` and `check_gen`.
- `core/crates/ployzd/src/docker/spec_store.rs`: `FloorTurn::admit` and `FloorTurn::check`.
- `core/crates/ployz-core/src/volume_switch.rs`: `SeedVolumeRequest` with its optional `gen`.
- `dashboard/src/modules/volume-run/volume-run.inngest.ts`: `seedVolume` and the gen it sends.
- `dashboard/src/modules/volume-placement/mirror-sync.server.ts`: `newestGen`.
- `dashboard/src/modules/volume-run/volume-run.server.ts`: `sendSwitch`.
- `dashboard/src/modules/volume-run/tables.ts`: the `gen` identity column.
- `core/crates/ployzd/src/volume_plugin/tests/seed_tests.rs`: `a_seed_replaces_a_slot_a_mirror_pull_marked_or_that_holds_no_point`, the designed replacement the model must keep.

Sync commit: `fc83111134412734a8e4801e36fb37d59f6df743`, the commit this model was checked against, and every line cited above is at it except the finding's step 7, which is at `0928c565975b97f0446d60298f30a3a5267f1a97`. Code in the list above that changed after it has not been rechecked.
