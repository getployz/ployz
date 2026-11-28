---
name: verify-volume-runs
description: Model-check Volume Runs (resize, Move, Release, mirror rm, a second Move, Restore, a Deploy's lane, late steps from a dead run, restarts and Machines away) on up to three Machines against DESIGN.md bet 6, and each Machine's refusal of an Operator's Volume start that creates a Container, with the Stateright model in core/crates/ployz-verify/src/volume_runs.rs. Use when a change touches `plan` or `expect_matches` in ployz-core's volume_copy.rs, `SetVolumeCopy` or `SetVolumeSize` handling in ployzd (machine/local_machine/volume_copy.rs, the `volume_floors` and `volume_forgotten` tables in docker/spec_store.rs, `refuse_forgotten` and `forgot_here` in docker/lifecycle.rs, `prepare_volumes`, `admit_plain_mount`, `start_volume_holder` and `cut_volume_copy` in machine/local_machine/container.rs, the switch lock (`lock_switch`, `wait_out_switch` and `SwitchGuard`) in machine/local_machine/switch.rs, volume_plugin/size.rs, the pull's `raise_slot_bound` and `pull_stream`, and `copy_destroy` with its `DestroyRequest`, in volume_plugin/copy.rs and ployzd's volume_copy.rs, a slot's `written_here` in volume_plugin/inspect.rs, volume_plugin/departure.rs), `admit_volume_start` in machine/local_machine/admission.rs or its call in machine_api/local.rs, `size_plan`, `SizePlan`, `SizeFacts` or `Refused` in ployz-decide's volume_size.rs, or the volume-run Inngest steps, Deploy lanes and Postgres gates in dashboard/src/modules/volume-run/. Also when a change touches a Volume's first copy: `first_copy` and `first_copy_on` in ployz's deploy/planning/volumes.rs, `FirstCopyUnfit`, or ployz-store's volume/first_copy.rs and its call in deployment/record.rs, which the model in core/crates/ployz-verify/src/first_placement.rs checks.
---

# Verify Volume Runs

The model in `core/crates/ployz-verify/src/volume_runs.rs` explores every interleaving of one resize of the Volume, a grow or a shrink, one Move after it, the Release that takes the Move over, and every `SetVolumeSize` and `SetVolumeCopy` a dead run already sent landing late. On three Machines it adds, one run at a time under the row lock: a `volume mirror rm`, a second Move to C with its own Release, a Machine away and forgotten, a Restore from the copy left, one Deploy's lane (Prepare, Create, a kept start), and a `ployzd` restart between any floor write and its plugin step. Prepare runs as two steps, one reading the forgotten record and one reading the copy, so a Forget could land between them, except that both run under the switch lock that Forget takes. Once its row ends, a run sends nothing new: each step checks ownership first. It calls the real `ployz_core::plan`, so a planner change is checked as written. Which run starts next is the model's copy of the dashboard planner. It includes `planMove`'s refusal of a Move while no Machine holds a Writer root, because a Release left the Volume only slots. Everything else (the Machine's lock and floor steps, the start gate `admit_volume_start`, `admit_plain_mount` and its lagging view, the forgotten-origin record and `refuse_forgotten`, departure, `set_size_admitted` with its shrink floor and the pull's bound raise, the Inngest drivers with `RESIZE_ORDER` and `undoOnRefusal`, `deleteMirror`, `restoreVolume`, `requireOwner` and the `commitMove` gate) is a hand copy of the code, and drifts unless you keep it in step.

With Cloud honest, the row lock and the owner checks carry one Writer through late promotes. Floors and the expected-copy check are defence in depth: removing either alone keeps `never two Writers` green on three Machines, and the floors break it only with the owner checks gone too, which the two-Machine test shows. The late Deploy steps are fenced as follows. Prepare has `admit_plain_mount` plus its own forgotten-record check, and it reads that record and the copy under one switch lock. Create refuses a fresh root of an origin the Machine forgot while it holds no copy.

## Run it

```
cd core && cargo test -p ployz-verify
```

A few minutes on a cold build, about 50 s warm (`cargo test -p ployz-verify volume_runs` runs only this model). CI runs it in the workspace nextest, so a red model blocks the PR.

The run is green when all thirty-five tests pass:

- `volume_runs_hold`: the code as it is, in each of the three two-Machine scenarios (below), in both directions. It prints each scenario's state count. Every property holds and every `sometimes` is reached. A `sometimes` that is never reached also fails here: it means the model can no longer reach that outcome, so a property passing beside it proves nothing.
- `volume_runs_hold_when_machines_refuse_created_starts`: the same with each Machine's start gate closing at any moment of the Move. The Move rechecks its start just before `commitMove` (`commitAndStart`), so a gate closed by then ends the Move warm and its Release starts the Service on A again. A gate that closes after the commit fails the forward Release, and a Deploy then repairs it (`Repair`): `fenceFollowed` promotes B with no start, and the Deploy's own Create starts the Service. Cloud's runs call as an Operator, and `admit_volume_start` refuses an Operator's `SetVolumeCopy` whose start creates a Container, before any floor is read; a start of a Container the Machine kept is served. A gate that closes before the first step stands for a Machine that refuses from the start. `a_machine_refuses_every_operator_start_that_creates_a_container` takes the gate out (`Broken::start_gate`) and expects "a Machine creates no Container an Operator's Writer start names" to fail.
- The twelve two-Machine `breaks` tests each remove one guard (a `Broken` flag) and expect the named properties to fail. One of these going green-to-red means a guard no longer earns its keep, or the model lost the path that shows it. Four are the Move's: `floors_let_a_release_win_over_late_steps`, `floors_and_owner_checks_keep_one_writer`, `the_cut_stops_the_service_before_its_flip` and `a_refusal_frees_the_switch_lock`. Eight are the resize's. For the grow: `a_grow_records_only_after_every_copy_holds_it`, `a_refused_grow_records_nothing`, and `a_move_needs_no_raise_of_its_own`, which shows that the pull's raise is what makes a Move to a Machine the grow never asked safe without a raise step of its own. For the shrink: `a_shrink_records_before_any_copy_lowers`, `a_shrink_lowers_the_primary_before_its_mirrors`, `a_shrink_refused_before_any_copy_changed_undoes_its_record`, and `a_refused_shrink_stops`. `a_done_resize_recorded_its_target` runs in both directions.
- `three_machine_runs_hold`: the code as it is on three Machines, in `TwoMoves`, `TwoMoves` with two rows Cloud ends early and no Machine away, and `Restore`. About 5.1M, 6.0M and 3.9M states.
- Six three-Machine breaks. Each expects "never two Writers" to fail.
  - `prepare_admits_only_a_plain_mount_on_the_writer` (`Broken::plain_mount`).
  - `prepare_makes_no_fresh_root_of_data_its_machine_forgot` (`Broken::prepare_forgotten`): a late Prepare after a mirror rm, reading a lagging view.
  - `prepare_reads_the_record_and_the_copy_under_one_switch_lock` (`Broken::prepare_switch`): a Forget lands between Prepare's two reads.
  - `a_machine_makes_no_fresh_root_of_data_it_forgot` (`Broken::forgotten`): a late Create after a Move and a mirror rm.
  - `no_move_starts_from_slots_alone` (`Broken::move_from_slots`, on `TwoMoves` with two rows ended early). A Release ends while its promote of A's slot is on the wire, a Move from that slot commits C, and the late promote makes A a second Writer.
  - `a_restored_copy_and_the_returning_old_one_are_the_accepted_window` (`exempt: false`). The old Writer that a Restore displaced comes back writable until it departs. This is bet 6's accepted window. The exemption follows that displaced copy, not its Machine. Once that copy is gone, a new root on the same Machine counts again.
- `a_departed_copy_survives_a_cloud_or_a_daemon_from_before_keep_written`: `Restore` with each `Skew`, every property and `sometimes`. `OldCloud` sends every Forget bare, so the Machine's refusal of a bare Forget of a slot `written_here` keeps the parked copy. `OldDaemon` has no Machine advertise `ployz.volume.keep-written.v1`: none reports `written_here` or reads `discard`, its plugin destroys a slot as its daemon planned, and its pull replaces a copy it can't send an incremental onto. Cloud then plans no full sync, and no mirror or Move there but a pull from the Writer's newest snapshot (`planFromCopies`' one check of every pull onto a slot, `mayReplace`), and its orphan sweep keeps every slot there. About 4.0M and 0.98M states.
- Nine breaks on `Restore` expect "only a person's mirror rm destroys a point only its Machine holds" to fail. In each, the displaced old Writer departs, parking its writes under a `dep-` snapshot no other Machine holds, and joins again.
  - `a_pull_keeps_a_departed_copy_its_writer_cannot_send_onto` (`Broken::pull_replaces_written`). A `volume mirror` onto the parked copy pulls from the restored Writer. The incremental finds no base, and without `written_here` in `pull_stream` the full send replaces the parked copy.
  - `a_full_sync_keeps_a_departed_copy` (`Broken::forget_ignores_written`). A `volume sync --full` onto the parked copy sends a bare Forget. The model's Cloud plans it onto any slot, so the Machine's `plan`, which refuses `WrittenHere`, is the only fence. Without the slot's `written_here`, the Forget destroys the parked copy.
  - `a_bare_forget_from_an_older_cloud_keeps_a_departed_copy` (`Broken::forget_opt_in` under `Skew::OldCloud`). 2f4f30f267 kept a slot `written_here` only for a Forget that said `keep_written`. An older Cloud's full sync says nothing, so the Forget destroys the parked copy.
  - `cloud_plans_no_full_sync_onto_a_machine_from_before_keep_written` (`Broken::sync_skips_capability` under `Skew::OldDaemon`). The older Machine destroys any slot a Forget names.
  - `cloud_plans_no_mirror_onto_a_departed_copy_on_a_machine_from_before_keep_written` (`Broken::mirror_skips_capability` under `Skew::OldDaemon`). The older Machine's pull replaces the parked copy with a full send.
  - `cloud_plans_no_move_onto_a_departed_copy_on_a_machine_from_before_keep_written` (`Broken::move_skips_capability` under `Skew::OldDaemon`). 4c6dc32563 checked the capability only in `mirror`'s planner, so a Move's warm pull replaces the parked copy.
  - `the_orphan_sweep_keeps_a_departed_copy` (`Broken::sweep_discards`). 4c6dc32563's sweep deleted every slot of a Volume gone from the config and said `discard` as a person's `mirror rm` does, so even a Machine with keep-written destroys the parked copy.
  - `the_orphan_sweep_keeps_every_slot_on_a_machine_from_before_keep_written` (`Broken::sweep_skips_capability` under `Skew::OldDaemon`). The older Machine destroys any slot the sweep's Forget names.
  - `cloud_plans_no_pull_onto_a_cut_a_restore_skipped_on_a_machine_from_before_keep_written` (`Broken::dep_prefix_only` under `Skew::OldDaemon`). This one parks no `dep-` snapshot. A's cut lands late, after A was forgotten and B's earlier copy restored, so A holds a cut's final no Writer has. 161c95ffc2's Cloud refused a pull onto an older Machine only for a `dep-` snapshot, so the mirror's full send replaces that final. The pull counts a cut's final a Restore skipped as off the Writer's line, like a departure's.
- Three breaks on `TwoMoves` expect "a mirror rm destroys only the copy its person confirmed, as they saw it" to fail. The mirror rm records each slot as its person confirmed it (`Confirmed`: snapshot, slot and `written_here`) when it is asked, and while it waits `Replace(m)` gives that slot another slot ID at the same snapshot and `Mark(m)` marks it written there.
  - `a_mirror_rm_deletes_only_the_copy_its_person_confirmed` (`Broken::discard_unbound`). 2ff6f2c021's run named the slot it found when it ran and said `discard`, so a slot replaced or written there after the confirm went too. `stillConfirmed` ends the run before any Forget instead.
  - `a_machine_keeps_a_confirmed_slot_written_there_since` (`Broken::expect_ignores_written`). Cloud's check passes, then the slot is marked before the Forget lands. Only the Machine's match of the expect's `written_here` keeps it.
  - `the_plugin_destroys_a_slot_only_as_the_daemon_planned_its_forget` (`Broken::destroy_unchecked`). The daemon plans the Forget under the switch lock, then a receive it started earlier, which runs outside that lock, lands (`Mark` or `Replace` while the request is `Destroying`). 161c95ffc2's plugin destroyed the slot by name. `copy_destroy` now plans the same Forget again under its mutation, once no receive runs, and keeps a slot that no longer matches.

The model was last confirmed to describe the code at PR #4571's review round 5, on top of `161c95ffc2002a8095c516eec3ddd68db1601c35`.

Each start names a direction. A grow starts with every copy and the Store at the small size and asks the large one; a shrink starts at the large size and asks the small one. The one Machine that can't take the target, if any, is `cramped`: a Pool with no room for the grow, or a copy that references too much for the shrink, whose `shrink_floor` then sits above it. The refuse order of a `SetVolumeSize` (shrink, switching, below the floor, equal, grow) is the real `ployz_decide::volume_size::size_plan`, which the model calls with the two sizes as 2 GB and 4 GB and a cramped copy referencing 2 GB, so `shrink_floor` is the real function; only the Pool's room (`NoCapacity`) is the model's own `fits`.

The full product of the resize and the Move is millions of states, so `Scenario` splits it into slices, each checked whole:

- `Resize`: the resize alone from every start, its row ended at any moment, with its resend and late requests.
- `ResizeThenMove`: a resize that runs to its own end, then the Move and its Release. The `breaks` tests run here unless they name another.
- `LateResize`: a resize whose row ends while a `SetVolumeSize` is on the wire. The Move starts at once, and the late request lands anywhere in it.
- `TwoMoves` (A, B, C): the first Move and its Release, the mirror rm, the second Move to C and its Release, and one Deploy at any moment the row is free.
- `Restore` (A, B, C): the first Move and its Release, a Machine away and forgotten, a Restore from the copy left, the Machine back and departing, that Machine joining again, then a `volume mirror`, a `volume sync --full`, a Move's warm round and the orphan sweep's Forget onto its parked copy, and one Deploy. A slot is `written_here` when it holds the departure's snapshot or once `Mark` stands for the cut, seed, promote and epoch marks that also set it on a real Machine. A Release whose bare Forget of a marked slot is refused keeps that slot and starts the Service all the same, and "a run that finished done still holds at rest" checks it. Only a person's `volume mirror rm` says `discard` in its Forget of a slot (`deleteMirror` with the row's `Discard`), and it names the slot as confirmed (`Expect::Confirmed`, `written_here` included); a Release's, a full sync's and the sweep's are bare. The sweep leaves out the Writer's removal, since a Machine plans a slot's Forget from the slot alone. `VolumeRuns::skew` runs it beside an older Cloud or older daemons (above).

The three-Machine slices run with no resends (`three`): a request a restart lost is sent again, and no other. A product with one resend per run is about 27M states, too slow for CI.

## Read a counterexample

A failure prints the property name and a `Path[n]` of actions:

- `Resize`, `Move`, `Release`: the driver takes its next step, sending a request when the step talks to a Machine. The resize's steps are `Set(A)`, `Set(B)`, `Record`, `Unrecord` and `Finish`; the state's `direction` says which way a `Set` goes.
- `ResizeResend`, `MoveResend`, `ReleaseResend`: the driver resends its outstanding request; the earlier copy may still land later.
- `EndResize`: Cloud ends the resize's row. Its `SetVolumeSize` still lands later, unfenced.
- `EndMove`: Cloud ends the Move's row, which spawns the Release.
- `Machine(i)`: request `i` in `State.requests` advances one step (admit, cut drain, stop, flip, start, reply; a `SetVolumeSize` runs in one step). The index is the request's position at that moment, not a stable id. A `SetVolumeSize` answer no driver waits on leaves the list when it lands, and the indices after it move down.
- `Crash(i)`: A's daemon restarts mid-flip of request `i`, losing every in-flight request on that Machine.
- `Enforce`: every Machine's start gate closes (only in the two start-gate tests).
- `Repair`: a Deploy follows a committed Move whose forward Release failed on a refused start. It promotes B with no start, unless B already writes at the committed epoch, then creates the Service there.
- On three Machines, `Move(k)`, `MoveResend(k)`, `EndMove(k)`, `Release(k)`, `ReleaseResend(k)` and `EndRelease(k)` name Move `k` (0 or 1) and its Release. `Next` starts the next run the row lock lets in. `Drop` and `EndDrop` are the mirror rm, `Restore` and `EndRestore` the Restore, `Mirror` the next step of the run after it (the mirror, the full sync, the Move's warm round or the sweep), `StartDeploy`, `Deploy`, `DeployResend` and `EndDeploy` the Deploy's lane. `Lagging(i)` runs request `i`'s Prepare against a view that lists no copy elsewhere. `Away(m)`, `Back(m)`, `Forget(m)` and `Depart(m)` take a Machine away, bring it back, forget it while away, and let it learn it was forgotten. `Replace(m)` and `Mark(m)` change a slot the mirror rm confirmed while it waits, or between the daemon's plan of its Forget and the plugin's destroy, and `Mark(m)` also marks any slot while a Release runs.

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

Bounds: on two Machines, one resize in one direction, between two sizes, before the Move, one Move and one takeover, one resend per run, one daemon crash, one Busy retry. The resize resends only in the `Resize` scenario. One Machine at most can't take the target, and a shrink floor never moves. On three Machines, no resize, no resends, one restart, one Deploy, one Machine away, and one row (two in one slice) Cloud ends early. A departed Machine joins again only once the Restore is over and nothing sent to it is in flight: the model's departure keeps a pulled slot that the real one destroys, so a late step landing after it joined again is out of scope. A bug that needs more of any of these is invisible.

Out of scope: operator collisions, a forged step, a Deploy that takes over a Move's lane (`fenceTaken`, and `fenceFollowed` except its repair of a refused start), the rowless mirror sync, a demote, warm rounds beyond one pull, a third takeover, a first Deploy's late step while no copy exists yet (every start has a Writer; `first_placement.rs` covers it, below), real Pool capacities and floors, ZFS, the Store's record transaction, a second resize, a settle (a second resize at the recorded size), a resize during or after a Move, and anything inside `plan`'s callers that the model does not copy. A green model says nothing about them; cover them with tests or `verify-server`.

## A Volume's first copy

`core/crates/ployz-verify/src/first_placement.rs` checks the case `volume_runs` leaves out: a Volume no Server holds yet, two Deploys of one Service, lanes that end at any moment with Prepare and Create still on the wire, a resend of each, Prepare's lagging view, a Server running out of room, and one Move. Each plan calls the real planner with the pin the Store lowered at claim. The Store's record (`volume::first_copy::place`), the Machines' Prepare and Create, and the Move are hand copies.

```
cd core && cargo test -p ployz-verify first_placement
```

About 55,000 states on the code as it is, a second or two warm. Six tests:

- `a_first_copy_starts_once`: "never two Writers", "a Deploy that read the pin starts the Volume only there" and "a moved Writer's registration is never refused" hold, and every `sometimes` is reached: a late step, the Store refusing a Preview that starts the Volume elsewhere, a Deploy waiting for a full pinned Server, and a Deploy registering a moved Writer.
- `a_pin_written_after_the_sends_starts_two`, `a_pin_an_outcome_retires_starts_two` and `a_record_that_lets_a_mismatched_preview_go_on_starts_two` each break "never two Writers".
- `a_planner_that_ignores_the_pin_strays_from_it` breaks only "a Deploy that read the pin starts the Volume only there": the record still refuses what it sends, so the planner's read is what keeps later Deploys from being refused. With the record's refusal gone too, `a_planner_that_ignores_the_pin_and_a_record_that_lets_it_go_on_start_two` breaks "never two Writers".

A planner that counts a moved Writer as a first copy also breaks "a Deploy that read the pin starts the Volume only there". Bounds: two Deploys, one resend each, one full Server, one Move, no Server away or forgotten, no mirror rm.
