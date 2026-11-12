---
name: verify-volume-runs
description: Model-check Volume Runs (resize, Move, Release, late steps from a dead run) against DESIGN.md bet 6, a Mirror's removal against its standing approval replayed, and each Machine's admission of an approved Move against its window, its own record and the source's decision and handoff receipt, with the Stateright models in core/crates/ployz-verify. Use when a change touches `plan` or `expect_matches` in ployz-core's volume_copy.rs, `SetVolumeCopy` or `SetVolumeSize` handling in ployzd (local_volume_copy.rs and its `withdrawn`, spec_store.rs floors, container.rs cut and start, volume_plugin/size.rs, the pull's `raise_slot_bound` in volume_plugin/copy.rs), `VolumeRecords` in ployz-decide's volume_records.rs, `admit_operator_copy`, `admit_operator_volume_start`, `record_cut`, `hand_off`, `checked_receipt` or `withdraw_volume` in ployzd's volume_approval.rs, `HandoffReceipt` or `verify_receipt` in ployz-core's authority/volume.rs, the receipt key in ployzd's sealing_key.rs, the send stream's grant check in volume_send.rs, or the volume-run Inngest steps and Postgres gates in dashboard/src/modules/volume-run/.
---

# Verify Volume Runs

The model in `core/crates/ployz-verify/src/volume_runs.rs` explores every interleaving of one resize of the Volume, a grow or a shrink, one Move after it, the Release that takes the Move over, and every `SetVolumeSize` and `SetVolumeCopy` a dead run already sent landing late. Once its row ends, the Move sends nothing new: each step checks ownership first. It calls the real `ployz_core::plan`, so a planner change is checked as written. Everything else (the Machine's lock and floor steps, `set_size_admitted` with its shrink floor and the pull's bound raise, the Inngest drivers with `RESIZE_ORDER` and `undoOnRefusal`, `requireOwner` and the `commitMove` gate) is a hand copy of the code, and drifts unless you keep it in step.

## Run it

```
cd core && cargo test -p ployz-verify
```

A few minutes on a cold build, about 30 s warm. CI runs it in the workspace nextest, so a red model blocks the PR.

The run is green when all twenty-four tests pass:

- `volume_runs_hold`: the code as it is, in each of the three scenarios (below), in both directions. It prints each scenario's state count. Every property holds and every `sometimes` is reached. A `sometimes` that is never reached also fails here: it means the model can no longer reach that outcome, so a property passing beside it proves nothing.
- `volume_runs_hold_under_enforcement`: the same with Device enforcement turning on at any moment of the Move. That Move was asked for before enforcement and carries no Volume approval. `an_enforcing_machine_refuses_every_operator_start` takes the start gate out and expects "an enforcing Machine starts no Container an Operator's Writer start names" to fail.
- `mirror_removal_holds` and `a_removed_mirror_withdraws_before_its_slot_goes`: the Mirror removal model (below).
- `move_admission_holds` and its seven `breaks` tests: the Move admission model (below).
- The other twelve `breaks` tests each remove one guard (a `Broken` flag) and expect the named properties to fail. One of these going green-to-red means a guard no longer earns its keep, or the model lost the path that shows it. Eight are the resize's. For the grow: `a_grow_records_only_after_every_copy_holds_it`, `a_refused_grow_records_nothing`, and `a_move_needs_no_raise_of_its_own`, which shows that the pull's raise is what makes a Move to a Machine the grow never asked safe without a raise step of its own. For the shrink: `a_shrink_records_before_any_copy_lowers`, `a_shrink_lowers_the_primary_before_its_mirrors`, `a_shrink_refused_before_any_copy_changed_undoes_its_record`, and `a_refused_shrink_stops`. `a_done_resize_recorded_its_target` runs in both directions.

Each start names a direction. A grow starts with every copy and the Store at the small size and asks the large one; a shrink starts at the large size and asks the small one. The one Machine that can't take the target, if any, is `cramped`: a Pool with no room for the grow, or a copy that references too much for the shrink, whose `shrink_floor` then sits above it.

The full product of the resize and the Move is millions of states, so `Scenario` splits it into slices, each checked whole:

- `Resize`: the resize alone from every start, its row ended at any moment, with its resend and late requests.
- `ResizeThenMove`: a resize that runs to its own end, then the Move and its Release. The `breaks` tests run here unless they name another.
- `LateResize`: a resize whose row ends while a `SetVolumeSize` is on the wire. The Move starts at once, and the late request lands anywhere in it.

## Mirror removal

`removal::MirrorRemoval`, in the same file, is its own small model (about 500 states, under a second). It checks that a removed Mirror stays removed when Cloud replays its standing approval. A Writer W and a Mirror M each hold their own `ployz_decide::volume_records::VolumeRecords`, and the model calls the real `admit`, `grant`, `live` and `withdraw` on them. Op 1, a Mirror approval past its window, starts admitted on both: W granted M every snapshot, and M holds a slot. A first admission of op 1 would be refused `Expired`, so it runs on the records alone. A hostile driver takes these steps in any order:

- `Write` on W, and `Snapshot` on W with op 1's approval (admit, then grant M, as `admit_operator_copy` does) or without one.
- `Pull` on M with op 1's approval. It lands W's newest snapshot only if W's `live` serves M that snapshot.
- `ForgetMirror`, M's `Forget`. M withdraws itself. Then `Destroy` takes the slot, unless `Restart` (M's daemon restarting) falls between. After a restart the withdrawal is on disk, the slot remains, and the `Forget` may be sent again.
- `ForgetWriter`, W's `Forget` naming M. W withdraws M and drops its run snapshots. Or `DropWriterForget`: Cloud never sends it.
- `ReadStream`, a read of W's send stream from M's shell.

A replayed approval is a `Snapshot` or a `Pull` with op 1 after the removal. Its properties:

- "once M's Forget has run, M never again holds a slot pulled under op 1". The Mirror's own withdrawal is what stops the data, whatever W still grants.
- "once both Machines recorded the removal, M gets no snapshot of W by pull or stream read". This leaves out a removal whose Writer `Forget` was dropped. There W's grant stands and a read from M's shell still gets W's snapshots. That is a residual of the plan, not a bug, and a `sometimes` shows the model reaches it.
- `sometimes`: M syncs a newer snapshot before the removal; a restart leaves M's withdrawal on disk with its slot in place; the removal completes on both Machines; it completes on M with W's `Forget` dropped; and then W's stream still serves M's shell.

`a_removed_mirror_withdraws_before_its_slot_goes` sets `Broken::removal_no_withdrawal`, so M's `Forget` destroys the slot without withdrawing. It expects the first property to fail. M's record still holds op 1 open, so a replayed pull lands a slot while W still grants M. The second property holds even then, because W's own withdrawal fences it.

## Move admission

`admission::MoveAdmission` is its own small model (about 730 states, a few seconds). It checks one approved Move of a Volume from A to B, op 1, and a Restore of A that a person approved, op 2, against its window, each Machine's own record and A's decision. A and B each hold their own `VolumeRecords`, and the model calls the real `admit`, `grant`, `cut`, `stopped_by_cut`, `resume`, `handoff`, `promote` and `live` on them, and the real `MachineSigned::sign` and `verify_receipt` on the receipt. The window passes at any moment (`Expire`). A's daemon may restart whenever A holds a change it has not synced (`RestartA`), and reads back what it synced. A hostile driver sends any step to either Machine, in any order, again and again, and replays the receipt it has seen or a foreign one, signed under a key that is not the Cluster's:

- on A: `Snapshot`, a warm snapshot that grants B what A leaves newest; `Cut`, which records the running Container it stops and grants B its snapshot; `Resume(Service)` or `Resume(Planted)`, a Kept start of the cut's own Container or of one made before enforcement, which records `Resumed` and ends the Move on A; `Handoff`, which records `Handed` at A's newest snapshot, ends the Move on A and answers with the receipt.
- on A: `Restore`, op 2's promotion of A's copy at the cut's snapshot after the cut, which A's records admit as a new owner of the copy over a handoff. The person forgot B first, as the CLI and Ployz Cloud require, so B takes no step after it and no longer counts as a Writer. `EndRestore` records that op 2 landed and ends it, a step of its own so a restart can fall between them.
- on B: `Pull(carried)`, which lands A's newest snapshot only if A's send stream serves B; `Promote(carried)`, at the newest snapshot B pulled. Each carries no receipt, the one seen, or the foreign one. `PromoteUnapproved`, the promotion Cloud sends alone with no approval, refused before any record is read.

Properties:

- "A and B are never both the Writer in the Cluster". `a_promote_without_an_approval_is_refused` sets `Broken::restore_unapproved`, so B promotes on no approval at all. The model encodes that refusal, which lives in ployzd and its tests (`a_cut_a_promote_and_a_forget_of_a_copy_need_an_approval`); it checks what the refusal buys. `a_source_records_its_decision` sets `Broken::no_decision`, so the handoff records nothing and a resume follows it. `a_target_promotes_only_on_a_receipt` sets `Broken::promote_unreceipted`. `a_receipt_leaves_only_after_its_decision_is_synced` sets `Broken::receipt_before_sync`, so a restart loses the decision after the receipt left.
- "B becomes the Writer only at the cut's snapshot". `a_receipt_promotes_only_at_its_snapshot` sets `Broken::receipt_any_snapshot`, so B checks the receipt against the snapshot it names, not the one B holds.
- "A's decision never changes once recorded".
- "no Machine admits its first step of the Move after the window without A's receipt". `a_first_step_needs_the_window` sets `Broken::admits_expired`, so the approval's check ignores the window.
- "once A resumed, A takes no snapshot or cut under the Move, and B pulls nothing new". `an_ended_move_never_runs_again` sets `Broken::admits_ended`, so an ended record admits every step.
- "a resume starts only a Container the approved cut stopped".
- "a Restore whose end A recorded is never admitted again", read from A's real records, across restarts.
- `sometimes`: a Move admitted inside its window promotes B after it; B, with no record of the Move from inside its window, promotes on A's receipt; A resumes after the window; A refuses a resume after its handoff; B refuses a promotion with no receipt or the foreign one; a first step after the window is refused `Expired`; B refuses a promotion with no approval; a Restore makes A the Writer after its handoff.

Out of its scope: the payload digest of B's start (unit tests in ployzd's local_tests.rs), a second Move and a receipt replayed across Moves (ployzd's volume_approval_tests.rs and live lanes), the Mirrors that follow the Move, and withdrawals. The `VolumeRuns` scenarios still drive Cloud's own runs, with no records; this driver's any step in any order covers what Cloud could send at each Machine's door.

## Read a counterexample

A failure prints the property name and a `Path[n]` of actions:

- `Resize`, `Move`, `Release`: the driver takes its next step, sending a request when the step talks to a Machine. The resize's steps are `Set(A)`, `Set(B)`, `Record`, `Unrecord` and `Finish`; the state's `direction` says which way a `Set` goes.
- `ResizeResend`, `MoveResend`, `ReleaseResend`: the driver resends its outstanding request; the earlier copy may still land later.
- `EndResize`: Cloud ends the resize's row. Its `SetVolumeSize` still lands later, unfenced.
- `EndMove`: Cloud ends the Move's row, which spawns the Release.
- `Machine(i)`: request `i` in `State.requests` advances one step (admit, cut drain, stop, flip, start, reply; a `SetVolumeSize` runs in one step). The index is the request's position at that moment, not a stable id. A `SetVolumeSize` answer no driver waits on leaves the list when it lands, and the indices after it move down.
- `Crash(i)`: A's daemon restarts mid-flip of request `i`, losing every in-flight request on that Machine.

The Mirror removal and Move admission models' actions are the steps listed under their own headings. The checker hands jobs out in blocks, so a path can run past the first state that breaks a property. Find the first failing step before telling the story.

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

The Mirror removal model's bounds: one Writer and one Mirror, both enforcing, one approval already admitted on both, two writes, two sends of M's `Forget`, one restart of M's daemon between its withdrawal and its destroy, and one send of W's `Forget`. Out of its scope: a new approval after the removal, an approval a Machine never admitted (still new work while its window lasts), a second Mirror, a Machine that doesn't enforce, a pull with no approval, the source check on a pull's `from`, a `Forget` that `plan` refuses, and the approvals log's sync.

Out of scope: Deploy lanes, operator collisions, Machines that never answer, warm rounds beyond the one pull that brings in a B the resize never asked, a second takeover, real Pool capacities and floors, ZFS, the Store's record transaction, a second resize, a settle (a second resize at the recorded size), a resize during or after a Move, Restore, and anything inside `plan`'s callers that the model does not copy. A green model says nothing about them; cover them with tests or `verify-server`.
