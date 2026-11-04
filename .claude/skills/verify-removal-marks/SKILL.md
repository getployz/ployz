---
name: verify-removal-marks
description: Model-check the Config Store's removal mark lifecycle (remove, sweep, cancel, pull-request close, the removal Deployment ending) on Postgres and SQLite with the Stateright model in core/crates/ployz-verify. Use when a change touches core/crates/ployz-store/src/removal/ (due.rs sweep, settle, finish, close; mark.rs live, of, save, finish; mod.rs cancel_removal, stop, on_servers, purge), the row locks in scope.rs, the Deployment record path in deployment/lifecycle.rs, the transaction isolation in storage/pg.rs or storage/sqlite.rs, or Cloud's sweep triggers in dashboard/src/modules/config-store/.
---

# Verify removal marks

The model in `core/crates/ployz-verify/src/removal_marks.rs` explores every interleaving of one Environment's removal mark on the Config Store: its removal Deployment ending on the Servers, the sweep that finishes the mark and purges the Environment, the cancel that turns it into a tombstone, and a pull-request close that settles it. Each actor is one `Storage::write` transaction, and each of its steps is one SQL statement or one row lock of the function it copies. Postgres runs them at READ COMMITTED: a statement reads what is committed when it runs, a row update waits for the transaction holding the row, and a copy read earlier stays stale. SQLite takes the write lock at `BEGIN IMMEDIATE`, so there each transaction runs whole. Nothing calls the real Store; the model is a hand copy, and drifts unless you keep it in step.

The property, in one sentence: **a cancel that returned success keeps its Environment**. Once `cancel_removal` commits, the mark never reads `removed` and the Environment row is never purged. The harm when it breaks: a person cancels a removal, the Store says it is cancelled, and the next sweep deletes the Environment anyway, with its Services, Volumes and history.

## Run it

```
cd core && cargo test -p ployz-verify removal_marks -- --nocapture
```

Under a second warm; the whole crate takes about 40 s because of the Volume Runs model beside it. CI runs it in the workspace nextest, so a red model blocks the PR.

The run is green when all six tests pass. Read them as three groups:

- **Where the code holds.** `a_conditional_save_in_finish_keeps_a_committed_cancel` runs the sweep, a close, the cancel and the runner together: `finish` moves the mark to `removed` through `mark::finish` only `WHERE state = 'removing'`, and purges only when a row moved. `a_close_reads_the_mark_under_the_member_lock_and_keeps_a_cancel` (a close alone never undoes a cancel, because it reads the mark under the Environment lock) and `sqlite_runs_each_transaction_whole_and_keeps_a_cancel` (SQLite cannot race) check narrower setups. Each prints a state count and checks every property and every `sometimes`. A `sometimes` that is never reached fails here: the model can no longer reach that outcome, so a property passing beside it proves nothing.
- **The guards.** `finish_needs_its_conditional_save`, `close_needs_its_tombstone_check` and `cancel_needs_the_environment_lock` each drop a guard (a `Broken` flag) and expect the property to fail, printing the path. One of those going green-to-red means a guard no longer earns its keep, or the model lost the path that shows it. `finish_needs_its_conditional_save` is the finding below, kept out by the fix.

## Read a counterexample

A failure prints the property name, the model's flags, and a numbered story. Each row is one statement in the code's words, with the committed rows and the held row locks once it ran, for example:

```
 2. Sweep: mark::live reads every `removing` mark, under no lock (removal/due.rs sweep)
    mark=Removing environment=present removal=Applied held: project=free environment=free mark=free cancelled=false
```

`mark` is the committed `config_removal_mark.state`; `environment` says whether the `config_environment` row still exists; `removal` is the removal Deployment's status as `on_servers` reads it; `held` names who holds each row from its update to its commit; `cancelled` flips once `cancel_removal` returned success. `Record(status)` is the runner's `deployment::record` ending the removal Deployment with that status, one step because it takes the Project lock before anything else. `Whole(actor)` is a SQLite transaction running start to end.

Write the story out in the Store's words before deciding anything: which transaction read the mark, under which lock, and what had committed by then. A test that fails inside `assert_properties` prints the raw `Path[n]` of actions instead; to see the rows, call the module's `breaks` helper with that model in a scratch test, which prints the story.

If a picture would help, the pstack `/statechart` skill draws how a model moves between states and tells each shortest failing path in plain words. Refer to it by name; its commands change between releases.

## Real bug or model error

A counterexample is a **lead**, not a verdict. Confirm each step of the story against the function it names before calling it a bug. Each `Step` variant's doc comment names its file and function.

- If a step does something the code cannot, fix the model and say so. The usual suspects: a lock the model skips (`holder` decides which steps wait), a read the model takes from committed state where the code reuses a copy (or the other way round), and `records`, which lets the runner end the removal Deployment only while the Project row is free.
- If every step matches the code, it is a finding. Report it in plain words with the path. Keep the property and the model as they are; weakening either to go green hides the bug.

The finding this model was built on, now fixed: `removal::sweep` reads every `removing` mark with `mark::live` under no lock. `settle` then takes the Environment row, and `finish` used to save the copy it read as `removed` through `mark::save`'s unconditional upsert, and purge. A `cancel_removal` that took the Environment row first and committed `cancelled` in between was overwritten, and the Environment it had just kept was deleted. SQLite cannot reach this: both transactions take the database write lock at `BEGIN`. The window on Postgres was real: Cloud sweeps right after an applied removal Deployment ends and every hour, with no lock around user commands. `a_sweep_queued_behind_a_cancel_keeps_the_environment_*` in `core/crates/ployz-store/tests/removal.rs` reproduced it on the real Store. `mark::State` now has no `removed`, so `mark::save` cannot write it, and `mark::finish` moves a mark there only from `removing`. `finish_needs_its_conditional_save` sets `Broken::finish_saves_unconditionally` and prints the old path.

## A break that stays green

Guards overlap, so removing one can leave every property holding. Before calling a guard dead, rerun with a neighbouring `Broken` flag set too. Two pairs overlap here:

- `close_needs_its_tombstone_check` sets `finish_saves_unconditionally` as well. With `mark::finish` intact, a close past a tombstone moves nothing and purges nothing, so the property holds. The check still earns its keep in the code, where a close past a tombstone would also cancel the Branch's Deployments and list it due, which this model does not cover.
- `cancel_needs_the_environment_lock` runs on the fixed `finish`. With the upsert, the sweep breaks the property on its own, so the cancel's lock shows its worth only once `finish` is fixed. That is why the conditional `finish` is not the whole guard: the cancel must still lock the Environment, or it commits `cancelled` over a purge.

## Keep the model in step

When a change intentionally alters the lifecycle (a new guard, a new statement, a reordered lock, a new caller of `finish`), change the model in the same PR:

1. Update the matching `Step`, the program constant (`SWEEP`, `CLOSE`, `CANCEL`), or `step`'s arm, citing the file and function it copies.
2. If the change adds a guard, add a `Broken` flag that removes it and a `breaks` test naming the property it keeps. A guard with no failing variant is untested.
3. If the change fixes a finding, flip its finding test into a `Broken` flag that restores the old code and a `breaks` test, as `finish_needs_its_conditional_save` did. Do not delete it: its printed path is the record of what the fix keeps out.
4. If a property's meaning changes, change its text and say why in the PR body. Narrow a property only when the code is right and the property was wrong about the design.
5. Move the sync commit below forward only after you confirmed each covered function still reads as the model says.

## Bounds and blind spots

Bounds: one Project with one Environment target, one mark, one removal Deployment that starts `Running` or already `Applied`, one sweep, one close, one cancel, each running at most once, and the runner ending the Deployment once. The runner ends a `Running` removal `Applied`, and a `Cancelling` one either `Applied` (its run succeeded) or `Cancelled` (it failed), as `record` and `end` do; so a cancel can return success while the removal still applies on the Servers. A `Running` removal that ends `Failed` is out of bounds: the Environment stays on the Servers, and no finisher gets past `on_servers`. Three row locks: the Project, the Environment, the mark. No lock timeout: a transaction waits as long as it takes, where Postgres gives up after ten seconds and the write fails.

Out of scope: a Project-target mark and its member Environments, two sweeps side by side, a `remove()` run again after a cancel, a queued removal Deployment that never claimed, Cloud admitting what the sweep lists as due, the runner's own row lock on the Deployment, the `evidence` and `accepted` columns the same upsert overwrites, `clear_tombstone` on the next `remove()`, pull-request idle closes, and the Store's change log. A green model says nothing about them; cover them with the removal tests in `core/crates/ployz-store/tests/removal.rs`, on Postgres with `PLOYZ_STORE_TEST_POSTGRES` set.

## Coverage

Each model step is a copy of code at the sync commit. Reread these when a diff touches them:

- `core/crates/ployz-store/src/removal/due.rs`: `sweep`, `settle`, `finish`, `close` and its `Cancelled` early return.
- `core/crates/ployz-store/src/removal/mark.rs`: `live`, `of`, `save` and its `ON CONFLICT` update, `finish` and its `WHERE state = 'removing'`.
- `core/crates/ployz-store/src/removal/mod.rs`: `cancel_removal`, `stop`, `lock_project`, `on_servers`, `ran`, `purge`.
- `core/crates/ployz-store/src/scope.rs`: `environment`, `lock`, `lock_all`, `lock_id`, `owned`, `load_by_id`.
- `core/crates/ployz-store/src/deployment/lifecycle.rs`: `cancel`, `record` and its `Applied`/`Failed` choice, `end` and its `(Cancelling, Failed)` to `Cancelled` mapping.
- `core/crates/ployz-store/src/deployment/history.rs`: `in_flight`, `last_ran`.
- `core/crates/ployz-store/src/storage/pg.rs`: `begin` and its READ COMMITTED level, `TIMEOUT`.
- `core/crates/ployz-store/src/storage/sqlite.rs`: `transaction` and `BEGIN IMMEDIATE`.
- `core/crates/ployz-store/src/pull_request/mod.rs`: the calls to `removal::close` and `removal::sweep`.
- `dashboard/src/modules/config-store/store-github.inngest.ts` and `store-deployment.inngest.ts`: when Cloud sweeps.

Sync commit: `379d927a88e00ad82aafa355f828f3b4a0bf5451`, the commit that made `finish` conditional, checked against it. Code in the list above that changed after it has not been rechecked.
