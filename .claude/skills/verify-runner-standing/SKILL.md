---
name: verify-runner-standing
description: Model-check how a Deployment's runner reads its standing in the Config Store (the status() polls every CANCEL_POLL inside connecting and renewing, renewing's abort, lost_to_another_runner, the gate before fill_ingress, the Executed record) and how the store keeps the run's outcome and shows its deleted Volumes, against a user's cancel, a newer claim taking the Environment over, and forget_cluster's cancel_in_flight, with the Stateright model in core/crates/ployz-verify. Use when a change touches core/crates/ployz/src/sdk/store_runner.rs (run_deployment_expecting, execute, connecting, renewing, CANCEL_POLL, lost_to_another_runner, status, interrupted, fill_ingress, remove_volumes' call), core/crates/ployz-store/src/deployment/lifecycle.rs (record's Alive, Prepared, Executed, NotExecuted and Abandoned arms, claim's takeover and re-claim, cancel, cancel_in_flight, finish, end, the removed_volume projection), core/crates/ployz-store/src/deployment/mod.rs (running, lapsed), core/crates/ployz-core/src/config/runtime_outcome.rs (how an Interrupted outcome projects), or core/crates/ployz/src/sdk/store_call.rs.
---

# Verify the runner's standing

The model in `core/crates/ployz-verify/src/runner_standing.rs` explores every interleaving of one Deployment's run with the Config Store that holds its status. The runner claims the Deployment (`core/crates/ployz/src/sdk/store_runner.rs:177-180`), dials under `connecting` (`:195-218`), then plans, or builds and prepares, under `renewing` (`:733-746`, `:882`, `:1041`). It records the Deploy Preview (`:751-753`), executes under `renewing` (`:764-768`), deletes the Volumes the Deploy removes (`:776-781`), fills ingress (`:782-785`, `:807-809`) and records what executing did (`:786-791`).

While it awaits a piece of work, `connecting` and `renewing` call `status()` every `CANCEL_POLL`, 2 s (`:40`, `:1190-1198`, `:1208-1220`), and the work can end before the next call (`:1199`, `:1211`). `execute` also calls it once after executing, in `lost_to_another_runner` (`:771`, `:1367-1369`), and once at the gate before `fill_ingress` (`:782`). `status()` is `record(RunEvidence::Alive)` (`:1373-1377`), and each site compares the answer with its own set of statuses (`:1195`, `:1214-1217`, `:1368`, `:782`). The model gives each call its own step.

Meanwhile the store takes three steps. A user's `cancel` moves Running to Cancelling (`core/crates/ployz-store/src/deployment/lifecycle.rs:1471`). Another Deployment's `claim` sets this one unknown and leaves its runner column (`lifecycle.rs:1320-1325`). `forget_cluster` (`core/crates/ployz-store/src/teardown.rs:458`) runs `cancel_in_flight`, which sets it Cancelled (`lifecycle.rs:1502`).

The model also copies what the store shows of the deleted Volumes. `removed_volume` reads them Removed only for a Success outcome, and NotAttempted for any other (`lifecycle.rs:2146-2168`, the check at `:2158-2162`). An Interrupted outcome projects to Failed with reason Interrupted (`core/crates/ployz-core/src/config/runtime_outcome.rs:208-220`).

Nothing calls the real runner or store. The model is a hand copy, and it drifts unless you keep it in step. `ployz-verify` dev-depends only on `ployz-core` and `stateright` (`core/crates/ployz-verify/Cargo.toml`). So a product change leaves the model green, without even recompiling it, until you copy the change in.

Each store call is one step, because `record` (`lifecycle.rs:1512`), `claim` (`:1231`), `cancel` (`:1459`) and `cancel_in_flight` (`:1486`) each take one transaction. A record that follows a read, or the end of a piece of work, is one step with it, except the `Executed` record.

What `status()` returns, as the model reads it:

- `Ok(Running)` or `Ok(Cancelling)`: the Alive arm calls `running` (`lifecycle.rs:1533-1539`), which passes both (`core/crates/ployz-store/src/deployment/mod.rs:1211-1213`).
- `Ok(Unknown)`: the Alive arm skips `running` for an Unknown Deployment (`lifecycle.rs:1534`). The owner check still passes, since the takeover leaves the runner column (`lifecycle.rs:1522-1524`, `1320-1325`).
- `Err(conflict "This Deployment already ended")`: `running` refuses an Applied, Failed or Cancelled one (`mod.rs:1219-1221`, `1294-1296`).
- `Err(unavailable)`: `store_call` gave up waiting or running (`core/crates/ployz/src/sdk/store_call.rs:41-56`). The model allows one such failure (`failing`).

The properties, each one sentence:

- `TAKEOVER_KEEPS_OUTCOME`: **a run never returns with its `Executed` refused because the Deployment went Unknown.** This is finding 1. The store keeps no outcome for a run that executed (`lifecycle.rs:1615-1618`).
- `DELETIONS_KEPT`: **once the run returned, the store shows the Volumes it deleted as removed, unless `cancel_in_flight` ended the Deployment.** This is finding 1 with Volumes deleted. `cancel_in_flight` is exempt because `forget_cluster`, which runs it, lets go of the Organization's Applied State and Volumes' fixed storage (`teardown.rs:442-445`, `458-469`).
- `NO_WORK_AFTER_END`: **once a read said the run is over, the runner starts no work, and keeps on none but deleting Volumes.** "Over" is Unknown or ended, the two `running` refuses (`mod.rs:1214-1221`). Deleting is exempt from "keeps on" because its abort is `|| ()` by design (`store_runner.rs:777-778`). This is finding 2.
- `HEEDS_CANCEL`: **while its last read said Cancelling, the runner starts or keeps on no work but deleting Volumes.** Deleting is exempt because the code never stops it on a cancel: the one read before `remove_volumes`, `lost_to_another_runner`, acts only on Unknown (`store_runner.rs:771`, `1367-1369`), and its abort is `|| ()` (`:778`).
- `TAKEN_OVER_INTERRUPTED`: **the store keeps no outcome but Interrupted on a Deployment that went Unknown.** This is what `record`'s takeover refusal keeps (`lifecycle.rs:1615-1618`).

## Run it

```
cd core && cargo nextest run -p ployz-verify runner_standing --no-capture
```

A warm run of the sixteen tests takes about a second. The code as it is has 667 states with one failed read, and the fix 542 (`current_code_breaks_only_the_findings` and `a_latched_standing_and_a_converting_store_keep_the_outcome_and_the_deletions` print them). Each story prints under its test's name, in brackets above it (`first_break`). CI runs `ployz-verify` in its workspace nextest (`.github/workflows/ci.yml:151`, `:216`), so a red model blocks the PR.

The run is green when all sixteen tests pass. Read them as four groups.

- **The findings.** Four tests pass by finding a counterexample on the code as it is, and print it.
  - `current_code_loses_the_outcome_of_a_run_taken_over_after_its_last_read` prints the shortest path that breaks `TAKEOVER_KEEPS_OUTCOME`.
  - `current_code_loses_the_volumes_a_run_deleted_once_taken_over` prints the shortest path that breaks `DELETIONS_KEPT` with no cancel.
  - `current_code_keeps_working_after_cancel_in_flight_ended_the_run` prints the shortest path that breaks `NO_WORK_AFTER_END`, through an `Err` read as keep.
  - `current_code_works_on_after_a_read_said_unknown` takes `cancel_in_flight` out and breaks `NO_WORK_AFTER_END` through `Ok(Unknown)` read as keep: at `connecting`, and at the gate.

  `current_code_breaks_only_the_findings` checks the code as it is with one failed read. It expects those three properties to break, `HEEDS_CANCEL` and `TAKEN_OVER_INTERRUPTED` to hold, and every `COVERAGE` row that is not `fix_only` to be reached. When the product fix lands, model it as the code writes it, make it the default, and turn each finding test into a `Broken` flag that restores the old code, as `removal_marks.rs` `finish_needs_its_conditional_save` did.
- **The fix variant.** `a_latched_standing_and_a_converting_store_keep_the_outcome_and_the_deletions` runs `failing(fixed())`: every property holds and every `sometimes` is reached. Each `sometimes` is one `COVERAGE` row naming a branch the code handles. `every_sometimes_needs_what_it_names` takes each row's `omit` out of the same model and expects the row unreached. To print a path under the fix, pass `failing(fixed())` to `breaks`.
- **The code's guards.** Six tests each drop one guard the code has (a `Broken` flag) from `fixed()` and expect the property it keeps to fail.
  - `connecting_needs_its_cancel_check` drops the Cancelling check in `connecting` (`store_runner.rs:1195-1197`) and breaks `HEEDS_CANCEL`.
  - `renewing_needs_its_abort` drops `renewing`'s abort (`:1214-1219`) and breaks `HEEDS_CANCEL`.
  - `executing_needs_the_prepared_cancel_check` drops the check on `record(Prepared)`'s summary (`:754-758`) and breaks `HEEDS_CANCEL`.
  - `deleting_needs_the_read_after_executing` makes `lost_to_another_runner` read Unknown as keep (`:771-773`, `1367-1369`) and breaks `NO_WORK_AFTER_END`.
  - `fill_ingress_needs_the_gate` drops the gate's Cancelling check (`:782`) and breaks `HEEDS_CANCEL`.
  - `the_store_needs_its_takeover_refusal` drops `running` for a non-Interrupted `Executed` on an Unknown Deployment (`lifecycle.rs:1616-1618`) and breaks `TAKEN_OVER_INTERRUPTED`.
- **The fix's guards.** Three tests each drop one part of the fix and expect the property it keeps to fail. They are below, under The fix.

If a guard test goes from green to red, the guard no longer earns its keep, or the model lost the path that shows it.

## The fix

The fix needs a runner change and a store change. Without the store change, `TAKEOVER_KEEPS_OUTCOME` still breaks (`the_fix_needs_the_store_to_keep_a_late_executed`). Without the runner change, finding 2 stays, since both its causes are the runner's reads (`store_runner.rs:1195`, `1213-1219`, `782`).

The runner change: every read goes through one `Standing`, and each site stops on Lost or Ended. "Latches Lost" means the first `Ok(Unknown)` read sets Lost and no later read changes it, and likewise the first ended `Err` sets Ended (`Standing::after`). `connecting` returns, and `renewing` aborts, except during `remove_volumes`, whose abort stays a no-op. The read after executing records Interrupted, carrying the deleted Volumes, on Lost and returns on Ended. The gate skips `fill_ingress`, and the `Executed` record goes as Interrupted on Lost.

The store change has two parts. `record` keeps a non-Interrupted `Executed` for an Unknown Deployment as Interrupted, in the same transaction, where the code refuses it (`lifecycle.rs:1615-1618`). `removed_volume` shows the deleted Volumes for an Interrupted outcome, where the code reads them NotAttempted for anything but a Success (`lifecycle.rs:2158-2162`).

Each part has a `Broken` flag and a test that breaks the property it keeps on `fixed()`:

| Part of the fix | `Broken` flag | Test | Property it keeps |
|---|---|---|---|
| Lost sticks | `lost_does_not_stick` | `the_fix_needs_lost_to_stick` | `NO_WORK_AFTER_END` |
| `record` keeps a late `Executed` as Interrupted | `store_refuses_late_executed` | `the_fix_needs_the_store_to_keep_a_late_executed` | `TAKEOVER_KEEPS_OUTCOME` |
| The projection shows an Interrupted outcome's deletions | `projection_hides_interrupted_deletions` | `the_fix_needs_the_projection_to_show_interrupted_deletions` | `DELETIONS_KEPT` |

`the_fix_needs_lost_to_stick` is the test that shows Lost sticks. It runs `failing(...)`, since only a failed read can follow `Ok(Unknown)` with another answer: `actions` offers no store step once the Deployment is Unknown. `every_sometimes_needs_what_it_names` cannot show it, because with `lost_does_not_stick` set every `COVERAGE` row is still reached.

The six code-guard tests run on `fixed()` too. So a change to the fix, or a fix flag set alongside, can change the shortest path a code-guard test prints. Each of those tests asserts only the property and one field of the last state, such as `after_cancel` or `after_over`. Read the new story and check that the break still goes through the guard the test names.

## Read a counterexample

A failure prints the test name, the property, the model's flags, and a numbered story cut at the first state in BFS order that breaks the property, so the last row is the broken state. `breaks` keeps that first path with a visitor and stops at the first discovery (`finish_when(HasDiscoveries::AllOf(..))`). `breaks_where` keeps the first one that also passes a filter on the path, and explores the whole model. Each row is one step in the code's words, followed by the Deployment's row and the runner once it ran, for example:

```
 9. Runner: record(Executed) with its outcome, naming the deleted Volumes; the Deployment is Unknown and the outcome not Interrupted: running() refuses it; execute returns that Err (store_runner.rs execute, lifecycle.rs record)
    store: status=Unknown preview=true outcome=None shows_removed=false | runner: Returned last_read=Unknown removed=true
```

`status` is the Deployment's status. `Ended` stands for Applied, Failed or Cancelled. `preview` is whether `record(Prepared)` kept the Deploy Preview. `outcome` is what the store kept, `None` until a record ends the run. `shows_removed` is whether `removed_volume` reads the deleted Volumes Removed. `runner` is the phase in `run_deployment_expecting` and `execute`. A `renewing` phase carries whether its abort ran. `last_read` is what the last `status()` call, or `record(Prepared)`'s summary, said. `removed` is whether `remove_volumes` returned. Under the fix, `standing` is the latch. A row says `(under Broken)` where a flag skipped a guard, and `(the fix)` where the fix acted.

Write the story out in the code's words before deciding anything: which reads the runner made, what each said, and where the store's step landed between them. A test that fails inside `assert_properties` prints the raw path of actions instead. To see the rows, call the module's `breaks` helper with that model in a scratch test inside `runner_standing::tests`, since `breaks` is private there.

If a picture would help, the pstack `/statechart` skill draws how a model moves between states and tells each shortest failing path in plain words. Refer to it by name, since its commands change between releases.

## Real bug or model error

A counterexample is a **lead**, not a verdict. Confirm each step of the story against the function it names before calling it a bug. Each `Action` and `Branch` variant's doc comment names its file and function, and `reads_as` prints the statement each step copies.

- If a step does something the code cannot, fix the model and say so. Three places are the usual suspects. `actions` decides which steps are offered in each phase. `RunnerStanding::step` holds what each site does with its read. `RunnerStanding::executed` holds `record`'s `Executed` arm and the projection.
- If every step matches the code, it is a finding. Report it in plain words with the path. Keep the property and the model as they are, because weakening either to go green hides the bug.

Finding 1, confirmed step by step at the sync commit below, has a path of 10 rows (`current_code_loses_the_volumes_a_run_deleted_once_taken_over`).

1. The runner claims the Deployment, dials, and plans or builds and prepares (`store_runner.rs:177-180`, `195-218`, `733-746`). `record(Prepared)` keeps the preview, and its summary reads Running, so executing starts (`:751-758`, `764-768`).
2. Executing returns a Success. `lost_to_another_runner` reads `Ok(Running)`, which is not Unknown (`:771`, `1367-1369`). `remove_volumes` starts under `renewing` with the no-op abort `|| ()` (`:776-778`).
3. Another Deployment of the Environment is claimed. Its UPDATE sets this one unknown and leaves its runner column (`lifecycle.rs:1320-1325`).
4. `remove_volumes` returns the Volumes it deleted. The gate reads `Ok(Unknown)`, which is not Cancelling, so `fill_ingress` runs (`store_runner.rs:782-785`).
5. `record(Executed)` sends the Success, naming the deleted Volumes (`:786-791`). The Deployment is Unknown and the outcome not Interrupted, so `running` refuses it (`lifecycle.rs:1615-1618`, `mod.rs:1214-1218`). The arm returns before it writes anything (`lifecycle.rs:1595-1618`), and `execute` returns the error. Cloud's `stopStoreDeploymentRun` then sends `Abandoned` through `abandonDeployment` (`dashboard/src/modules/config-store/store-deployment.server.ts:187`, `core/crates/ployz-sdk/src/config_store.rs:358`), which changes nothing on an Unknown Deployment (`lifecycle.rs:1877-1879`).

No `status()` call fails on this path. The shortest path that breaks `TAKEOVER_KEEPS_OUTCOME` deletes nothing: a cancel lets the gate skip `fill_ingress`, and the takeover lands after the gate's read, so the store refuses a Failed outcome the same way. When the takeover lands before `lost_to_another_runner` instead, the run records Interrupted with no deletions (`store_runner.rs:771-773`, `1253-1264`). `tests/deploy_client/runner_cancellation.rs` `taken_over_mid_create` (`:120-164`) shows that case, with the takeover while the first run still executes.

Finding 2 has two causes, and a fix for one leaves `NO_WORK_AFTER_END` red through the other.

- **`Err` read as keep.** `current_code_keeps_working_after_cancel_in_flight_ended_the_run` has 4 rows. `cancel_in_flight` sets the Deployment Cancelled while the runner dials (`lifecycle.rs:1486-1507`). The dial completes and the prepare starts. `renewing`'s tick reads the ended `Err`, and "a failed read skips one check" (`store_runner.rs:1213-1219`), so the prepare goes on. `connecting`, `lost_to_another_runner` and the gate read `Err` as keep too (`:1195`, `1368`, `782`).
- **`Ok(Unknown)` read as keep.** `current_code_works_on_after_a_read_said_unknown` takes `cancel_in_flight` out, and no read fails. After a takeover, `connecting` reads `Ok(Unknown)` and dials on, since only Cancelling stops it (`:1195`), and the prepare starts: 4 rows. Its second path takes the takeover after `lost_to_another_runner`, and the gate reads `Ok(Unknown)` and fills ingress, since only Cancelling skips it (`:782`): 7 rows.

What the model assumes, so you can judge the lead. A takeover needs a newer Deployment of the same Environment claimed while this run holds it, which `claim` takes from Queued (`lifecycle.rs:1264-1271`, `1320-1325`). The model gives each read and each piece of work its own step, so a store step can land between any two. Neither finding needs a failed read.

### Staging finding 1 in real code

The `deploy_client` tests in `core/crates/ployz/tests/deploy_client/` run the real runner and store against fake Machines. To land the takeover while `remove_volumes` runs, hold the fake's `remove_volume` (`support.rs:1133`). The Deploy that takes over is the test's third, and it needs three things:

- `accept_volume_loss` naming the Volume (`core/crates/ployz-store/src/deployment/admit.rs:73-79`);
- the volume observation in `Trusted.volumes` (`core/crates/ployz-store/src/trusted.rs:31-34`), which `observe_volumes` builds (`store_runner.rs:232`);
- the `version` from the Conflict's `details.diff.version`, since a stale review refuses with `{"diff": review.view}` (`core/crates/ployz-store/src/review/mod.rs:869-873`), whose `DiffView.version` is at `review/mod.rs:46-47`.

## A break that stays green

Guards overlap, so removing one can leave every property holding. Before calling a guard dead, rerun with a neighbouring `Broken` flag set too, or with the other value of `fix`.

- On the code as it is, `TAKEOVER_KEEPS_OUTCOME`, `DELETIONS_KEPT` and `NO_WORK_AFTER_END` already break, so dropping one runner guard there adds at most `HEEDS_CANCEL`. That is why the guard tests run on the fixed model.
- Under the fix, dropping the read after executing (`check_ignores_lost`) keeps `TAKEOVER_KEEPS_OUTCOME` holding, because the store keeps the late `Executed` as Interrupted. Only `NO_WORK_AFTER_END` breaks, because `remove_volumes` starts after a read said Unknown.
- On the code as it is, dropping the store's takeover refusal (`store_takes_any_outcome`) makes `TAKEOVER_KEEPS_OUTCOME` and `DELETIONS_KEPT` hold, since nothing refuses and the kept Success shows its deletions. The finding needs that refusal, and the refusal is right: it keeps a taken-over run's outcome Interrupted (`TAKEN_OVER_INTERRUPTED`).

## Keep the model in step

When a change intentionally alters how the runner reads its standing or how the store answers, such as a new site, a site that reads another status set, a new store step that sets the status, a reordered record, or a projection that shows deletions differently, change the model in the same PR.

1. Update the model function that copies the product function you changed, citing the file and function it copies, and the `reads_as` line that prints it:
   - `connecting` (`store_runner.rs`): the `Poll` arm of `RunnerStanding::step`.
   - `renewing`: `RunnerStanding::tick` and `Phase::abort`.
   - `execute`: the `Connected`, `Prepare`, `Executed`, `SessionEnds`, `Check`, `Removed`, `Gate`, `Filled` and `Record` arms of `RunnerStanding::step`, and `RunnerStanding::start`.
   - `lost_to_another_runner`, `status` and `store_call`: `RunnerStanding::read`, with `Read` and `Call`.
   - `record`'s Alive arm (`lifecycle.rs`): `RunnerStanding::read`. Its Executed arm and `removed_volume`: `RunnerStanding::executed`. Its Abandoned arm: `RunnerStanding::abandoned`. Its NotExecuted arm and `finish`/`end`: `finish` and `RunnerStanding::not_executed`. The Prepared arm's `running`: the `Prepare` arm of `step`.
   - `running` (`mod.rs`): `running`.
   - `claim`'s takeover, `cancel` and `cancel_in_flight` (`lifecycle.rs`): the `Takeover`, `Cancel` and `CancelInFlight` arms of `step`, and when `actions` offers each.
2. If the change adds a guard, add a `Broken` flag that removes it and a `breaks` test naming the property it keeps. A guard with no failing variant is untested.
3. If the change ships the fix, model it as the code writes it and flip the finding tests as described under Run it. Do not delete them, since their printed paths are the record of what the fix keeps out.
4. If a property's meaning changes, change its text and say why in the PR body. Narrow a property only when the code is right and the property was wrong about the design.
5. If the change adds a branch, add a `COVERAGE` row naming it, with the `omit` that takes it out.
6. Move the sync commit below forward only after you confirmed each covered function still reads as the model says.

## Bounds and blind spots

These are the bounds. There is one Deployment and one runner. The Deploy deletes Volumes when it succeeds. Each store step happens at most once, since each moves the status out of the set that offers it. At most one `status()` call fails. Executing ends in a Success, a Failed outcome or a session that ended. An aborted piece of work always stops: an aborted prepare records `NotExecuted`, and an aborted execution ends Failed or with its session ended.

These are out of scope:

- a takeover or `cancel_in_flight` between a read and the record the model merges with it, such as `connecting`'s Cancelling read and its `NotExecuted` (`store_runner.rs:205`), which then fails
- `build`'s own records (`Build`, `Built`), the Deploy's warnings (`record_warnings`, `:775`), and what `fill_ingress` records about ingress (`:815-835`)
- the lease lapsing, which every read remaps to Unknown (`mod.rs:976-981`, `1092-1095`), after `LEASE` (600 s, `mod.rs:336`) with no record
- a second claim by the same runner after a preview, which also sets the Deployment Unknown and keeps its runner (`lifecycle.rs:1239-1250`)
- Cloud's `abandonDeployment` while the run is still going, and the Abandoned arm's `NotExecuted` for a run with no preview (`lifecycle.rs:1881-1894`)
- a dial that fails, a refused prepare, or a build that needs an upload
- a store call other than `status()` failing, such as `record(Prepared)`, whose error `?` returns (`store_runner.rs:751-753`)
- an abort that comes too late to stop its work
- the select order in `connecting` (`biased`, `:1193`) and `renewing`

A green model says nothing about them. Cover them with `cargo nextest run -p ployz sdk::store_runner` and `cargo nextest run -p ployz --test deploy_client runner_cancellation`.

## Coverage

Each model step is a copy of code at the sync commit. Reread these when a diff touches them:

- `core/crates/ployz/src/sdk/store_runner.rs`: `run_deployment_expecting` from the claim to `execute`. All of `execute`: the plan or build and prepare, `record(Prepared)` and its Cancelling check, executing under `renewing`, `lost_to_another_runner`, `remove_volumes` with its no-op abort, the gate, the `Executed` record and `Abandoned`. Also `fill_ingress`'s `renewing`, `prepare`'s and `build`'s `renewing`, `connecting`, `renewing`, `interrupted`, `not_executed`, `lost_to_another_runner`, `status`, and `CANCEL_POLL`.
- `core/crates/ployz/src/sdk/store_call.rs`: `store_call`, `bounded` and `unavailable`.
- `core/crates/ployz-store/src/deployment/lifecycle.rs`: `claim`'s re-claim and takeover UPDATE. `cancel` and `cancel_in_flight`. `record`'s owner check, lease renewal, and its Alive, Prepared, Executed (the `taken_over` refusal and keep), NotExecuted and Abandoned arms. Also `finish`, `end`, and the `removed_volume` projection.
- `core/crates/ployz-store/src/deployment/mod.rs`: `running`, `ended`, `lapsed` and the remap in `stored`.
- `core/crates/ployz-core/src/config/runtime_outcome.rs`: the Interrupted arm of the projection.
- `core/crates/ployz-store/src/teardown.rs`: `forget_cluster`'s call to `cancel_in_flight`.

Sync commit: `d44c048596334df069b0c5ff27c120e357f1edd0`, the `main` this model was checked against, and every line cited above is at it. Code in the list above that changed after it has not been rechecked.
