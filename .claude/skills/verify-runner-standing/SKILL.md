---
name: verify-runner-standing
description: Model-check how a Deployment's runner reads its standing in the Config Store (the standing() reads every CANCEL_POLL inside connecting and renewing, renewing's abort, the read after executing, the gate before fill_ingress, the Run::over latch, and executed's record and resend) and how the store keeps the run's outcome and shows its deleted Volumes, against a user's cancel, a newer claim taking the Environment over, and forget_cluster's cancel_in_flight, with the Stateright model in core/crates/ployz-verify. Use when a change touches core/crates/ployz/src/sdk/store_runner.rs (run_deployment_expecting, execute, connecting, renewing, CANCEL_POLL, Standing, Over, the free standing fn, Run::standing, Run::over, executed, interrupted, fill_ingress, remove_volumes' call), core/crates/ployz-store/src/deployment/lifecycle.rs (record's Alive, Prepared, Executed, NotExecuted and Abandoned arms, claim's takeover and re-claim, cancel, cancel_in_flight, finish, end, the removed_volume projection), core/crates/ployz-store/src/deployment/mod.rs (running, ended, owned_elsewhere, lapsed), core/crates/ployz-core/src/config/runtime_outcome.rs (how an Interrupted outcome projects), or core/crates/ployz/src/sdk/store_call.rs.
---

# Verify the runner's standing

The model in `core/crates/ployz-verify/src/runner_standing.rs` explores every interleaving of one Deployment's run with the Config Store that holds its status. The runner claims the Deployment (`core/crates/ployz/src/sdk/store_runner.rs:178`), dials under `connecting` (`:191-215`), then plans, or builds and prepares, under `renewing` (`:795-808`, `:953`, `:1115`). It records the Deploy Preview (`:813-824`), executes under `renewing` (`:830-834`), deletes the Volumes the Deploy removes (`:849-854`), fills ingress (`:855-861`, `:869-881`) and records what executing did through `executed` (`:862`, `:1336-1365`).

While it awaits a piece of work, `connecting` and `renewing` call `standing()` every `CANCEL_POLL`, 2 s (`:40`, `:1259-1275`, `:1277-1292`), and the work can end before the next call (`:1272`, `:1284`). `execute` also calls it once after executing (`:837-846`) and once at the gate before `fill_ingress` (`:855`). `Run::standing` (`:1466-1478`) returns the latch `Run::over` (`:699-706`) once it is set, and otherwise sends `record(RunEvidence::Alive)` and reads the answer through the free fn `standing` (`:733-749`):

| The read | `Standing` | Latches |
|---|---|---|
| `Ok(Running)` | `Keep` | no |
| `Ok(Cancelling)` | `Cancel` | no |
| `Ok(Unknown)` | `Over(Lost)` | yes |
| `Ok` of any ended or not-yet-run status | `Over(Ended)` | yes |
| `Err` with code Conflict | `Over(Ended)` | yes |
| any other `Err` | `Unread` | no |

Every site stops on `Cancel` and `Over`, and goes on on `Keep` and `Unread`. The read after executing is the exception: on `Cancel` it goes on, since deleting is never interrupted, and on `Over(Lost)` it records Interrupted (`:838-840`, `:1321-1334`). `execute` reads `record(Prepared)`'s answer through the same `standing` (`:813-824`), with no latch. The model gives each read its own step.

`executed` sends `record(Executed)`. When the store refuses it as a Conflict and the run attempted no Volume deletion (every removal `Omitted`; a `Failed` removal counts as attempted), it sends the outcome again as Interrupted and returns that answer, or the first refusal if the store refuses again (`:1349-1364`). The store answers that resend in one of three ways:

- **New evidence on an Unknown Deployment: kept.** An Unknown Deployment refuses any outcome but Interrupted (`core/crates/ployz-store/src/deployment/lifecycle.rs:1641-1644`), keeps the Interrupted one, and stays Unknown (`:1789-1799`). This is the case the resend is for.
- **A Conflict that turns on what the store holds: refused again.** Another runner (`lifecycle.rs:1544-1546`), no preview (`:1635-1640`), and an ended Deployment through `finish` (`:2344-2353`) don't depend on the outcome sent, so the resend meets them again. `executed` then returns the first refusal.
- **An identical replay: accepted, and nothing changes.** "Outcome already recorded" compares a digest of the outcome and removals sent (`:1622-1634`), and returns `Ok` for an exact replay before it checks the status. A Deployment that already holds this same Interrupted outcome, ended or Unknown, so takes the resend as a replay. That changes nothing: it returns the stored summary and writes nothing.

The store does not refuse Interrupted from a Running or Cancelling Deployment: `finish` keeps it and ends the Deployment Failed, or Cancelled if it was Cancelling (`:1800`, `:2344-2354`, `:2365-2367`). The resend never gets there, though. It follows only a Conflict, and the only Conflicts on `Executed` for a Running or Cancelling Deployment are another runner and no preview, since every write of an outcome also ends the Deployment or leaves it Unknown (`:576-577`, `:1795-1796`, `:2370-2371`). Both of those refuse the resend too. A future Conflict on `Executed` that depends on the outcome sent would break this: the resend would then end a running Deployment Failed, or Cancelled, as Interrupted. Check for that before adding one.

Meanwhile the store takes three steps. A user's `cancel` moves Running to Cancelling (`lifecycle.rs:1494`). Another Deployment's `claim` sets this one Unknown and leaves its runner column (`lifecycle.rs:1320-1332`). `forget_cluster` (`core/crates/ployz-store/src/teardown.rs:458`) runs `cancel_in_flight`, which sets it Cancelled (`lifecycle.rs:1525`).

The model also copies what the store shows of the deleted Volumes. `removed_volume` reads them Removed only for a Success outcome, and NotAttempted for any other (`lifecycle.rs:2174-2196`, the check at `:2186-2190`). An Interrupted outcome projects to Failed with reason Interrupted (`core/crates/ployz-core/src/config/runtime_outcome.rs:208-220`).

Nothing calls the real runner or store. The model is a hand copy, and it drifts unless you keep it in step. `ployz-verify` dev-depends only on `ployz-core` and `stateright` (`core/crates/ployz-verify/Cargo.toml`). So a product change leaves the model green, without even recompiling it, until you copy the change in.

Each store call is one step, because `record` (`lifecycle.rs:1534`), `claim` (`:1232`), `cancel` (`:1485`) and `cancel_in_flight` (`:1509`) each take one transaction. A record that follows a read, or the end of a piece of work, is one step with it, except the `Executed` record. `executed`'s record and its resend are one step: within the model's bounds, which have no other runner and always a preview, the store refuses the first only for a Deployment that is Unknown or ended, and no store step leaves either.

What `record(RunEvidence::Alive)` returns, as the model reads it:

- `Ok(Running)` or `Ok(Cancelling)`: the Alive arm calls `running` (`lifecycle.rs:1555-1565`), which passes both (`core/crates/ployz-store/src/deployment/mod.rs:1805-1807`).
- `Ok(Unknown)`: the Alive arm skips `running` for an Unknown Deployment (`lifecycle.rs:1556-1559`). The owner check still passes, since the takeover leaves the runner column (`lifecycle.rs:1544-1546`, `1320-1332`).
- `Err(conflict "This Deployment already ended")`: `running` refuses an Applied, Failed or Cancelled one (`mod.rs:1813-1815`, `1888-1890`).
- `Err(unavailable)`: `store_call` gave up waiting or running (`core/crates/ployz/src/sdk/store_call.rs:41-56`). The model allows one such failure (`failing`).

The properties, each one sentence:

- `TAKEOVER_KEEPS_OUTCOME`: **a run never returns with its `Executed` refused because the Deployment went Unknown.** This is finding 1. Without the resend, the store keeps no outcome for a run that executed (`lifecycle.rs:1641-1644`).
- `DELETIONS_KEPT`: **once the run returned, the Deployment's nodes show the Volumes it deleted as Removed, unless `cancel_in_flight` ended the Deployment.** This is finding 1 with Volumes deleted. It checks only the `removed_volume` node projection (`lifecycle.rs:2174-2196`), not Applied State. `cancel_in_flight` is exempt because `forget_cluster`, which runs it, lets go of the Organization's Applied State and Volumes' fixed storage (`teardown.rs:442-445`, `458-469`).
- `NO_WORK_AFTER_END`: **once a read said the run is over, the runner starts no work, and keeps on none but deleting Volumes.** "Over" is Unknown or ended, the two `running` refuses (`mod.rs:1808-1815`). Deleting is exempt from "keeps on" because its abort is `|| ()` by design (`store_runner.rs:850-851`). This is finding 2.
- `HEEDS_CANCEL`: **while its last read said Cancelling, the runner starts or keeps on no work but deleting Volumes.** Deleting is exempt because the code never stops it on a cancel: the read before `remove_volumes` goes on on `Cancel` (`store_runner.rs:845`), and its abort is `|| ()` (`:851`).
- `TAKEN_OVER_INTERRUPTED`: **the store keeps no outcome but Interrupted on a Deployment that went Unknown.** This is what `record`'s takeover refusal keeps (`lifecycle.rs:1641-1644`).

## Run it

```
cd core && cargo nextest run -p ployz-verify runner_standing --no-capture
```

A warm run of the nineteen tests takes about a second. The model has four shapes, built in the tests module:

- `as_is()`: the code as it is.
- `as_was()`: the code before `Standing`, which is `as_is` with four `Broken` flags set: `latch_does_not_stick`, `ended_reads_as_keep`, `unknown_reads_as_keep` and `no_resend`.
- `store_fixed()`: the code as it is with a store change the code does not have (`store_fix`), described under What is still open.
- `failing(model)`: any of them with one `standing()` read that may fail.

With one failed read, the code as it is has 526 states, the code as it was 669, and the store fix 526. `the_code_loses_an_outcome_only_after_deleting_volumes`, `the_code_as_it_was_breaks_only_the_findings` and `a_converting_store_would_keep_the_outcome_and_the_deletions` print the counts. Each story prints under its test's name, in brackets above it (`first_break`). CI runs `ployz-verify` in its workspace nextest (`.github/workflows/ci.yml:151`, `:216`), so a red model blocks the PR.

The run is green when all nineteen tests pass. Read them as five groups.

- **What is left of finding 1.** `the_code_still_loses_the_volumes_a_run_deleted_once_taken_over` passes by finding a counterexample on the code as it is, and prints it: a takeover after the read that let `remove_volumes` start. Read it under What is still open.
- **The whole model.**
  - `the_code_loses_an_outcome_only_after_deleting_volumes` runs `failing(as_is())`. `NO_WORK_AFTER_END`, `HEEDS_CANCEL` and `TAKEN_OVER_INTERRUPTED` hold. `TAKEOVER_KEEPS_OUTCOME` and `DELETIONS_KEPT` break, and every state that breaks them (`every_break`) has Volumes deleted. Every `COVERAGE` row that is not `StoreFix` is reached.
  - `the_code_as_it_was_breaks_only_the_findings` runs `failing(as_was())`. The three properties the findings name break, the other two hold, and every `Always` row is reached.
  - `a_converting_store_would_keep_the_outcome_and_the_deletions` runs `failing(store_fixed())`. Every property holds and every `sometimes` is reached.
  - `every_sometimes_needs_what_it_names` takes each `COVERAGE` row's `omit` out of the model that reaches it, and expects the row unreached. Each `sometimes` is one row naming a branch the code handles, and its `reach` says which models have it: `Always` (every shape), `Standing` (the code as it is, with or without the store fix), `Resend` (the code as it is without the store fix, since the store fix never refuses a lost run's `Executed`), or `StoreFix`.
- **What `Standing` added.** Six tests each drop one part `Standing` added from `as_is()` and expect the property it keeps to break. The first four flags together make `as_was()`, so each of those stories is the finding its part closes.

  | Part of the code | `Broken` flag | Test | Property it keeps |
  |---|---|---|---|
  | `executed` sends a refused outcome again as Interrupted (`store_runner.rs:1359-1361`) | `no_resend` | `the_code_needs_to_send_a_refused_outcome_again` | `TAKEOVER_KEEPS_OUTCOME` |
  | `standing` reads a Conflict as `Over(Ended)` (`:747`) | `ended_reads_as_keep` | `the_code_needs_an_ended_read_to_stop_it` | `NO_WORK_AFTER_END` |
  | `connecting` and the gate stop on `Over(Lost)` (`:1270`, `:860`) | `unknown_reads_as_keep` | `the_code_needs_connecting_and_the_gate_to_stop_on_lost` | `NO_WORK_AFTER_END` |
  | `Run::over` latches the first `Over` read (`:1469-1477`) | `latch_does_not_stick` | `the_code_needs_its_latch_to_stick` | `NO_WORK_AFTER_END` |
| The read after executing stops on `Over(Ended)` (`:841-844`) | `check_ignores_ended` | `deleting_needs_the_read_after_executing_to_stop_on_ended` | `NO_WORK_AFTER_END` |
| The gate skips `fill_ingress` on `Over(Ended)` (`:860`) | `gate_ignores_ended` | `fill_ingress_needs_the_gate_to_stop_on_ended` | `NO_WORK_AFTER_END` |

  `the_code_needs_its_latch_to_stick` runs `failing(...)`, since only a failed read can follow Lost or Ended with another answer: `actions` offers no store step once the Deployment is Unknown or ended. `every_sometimes_needs_what_it_names` cannot show the latch, because with `latch_does_not_stick` set every `COVERAGE` row is still reached.
- **The code's older guards.** Six tests each drop one guard from `as_is()` and expect the property it keeps to break.
  - `connecting_needs_its_cancel_check` drops `Cancel` stopping `connecting` (`store_runner.rs:1270`) and breaks `HEEDS_CANCEL`.
  - `renewing_needs_its_abort` drops `renewing`'s abort (`:1288-1289`) and breaks `HEEDS_CANCEL`.
  - `executing_needs_the_prepared_cancel_check` drops the `Cancel` check on `record(Prepared)`'s answer (`:817-821`) and breaks `HEEDS_CANCEL`.
  - `deleting_needs_the_read_after_executing` makes that read take `Over(Lost)` as keep (`:838-840`) and breaks `NO_WORK_AFTER_END`.
  - `fill_ingress_needs_the_gate` drops the gate's `Cancel` check (`:860`) and breaks `HEEDS_CANCEL`.
  - `the_store_needs_its_takeover_refusal` drops `running` for a non-Interrupted `Executed` on an Unknown Deployment (`lifecycle.rs:1642-1644`) and breaks `TAKEN_OVER_INTERRUPTED`.
- **The store fix's parts.** Two tests each drop one part of `store_fixed()`. They are under What is still open.

If a guard test goes from green to red, the guard no longer earns its keep, or the model lost the path that shows it.

Passing `breaks` an `always` that holds panics with "a path breaks the property" (`first_break`). To see a guard's break, pass the model with that guard dropped, as the tests above do. To print the first path that reaches a `sometimes`, pass its `COVERAGE` row's name to `breaks`, which then looks for a state where it holds: for example `breaks(as_is(), "record refuses a lost run's Executed and keeps it sent again as Interrupted")`.

## What is still open

Finding 1 is closed except when the run deleted Volumes. `the_code_still_loses_the_volumes_a_run_deleted_once_taken_over` prints 9 rows:

1. The runner claims, dials, and plans or builds and prepares (`store_runner.rs:178`, `191-215`, `795-808`). `record(Prepared)` keeps the preview, and its answer reads `Keep`, so executing starts (`:813-824`, `:830-834`).
2. Executing returns a Success. The read after executing says `Keep` (`:837-846`), and `remove_volumes` starts under `renewing` with the no-op abort `|| ()` (`:849-851`).
3. Another Deployment of the Environment is claimed. Its takeover saves this one Unknown and leaves its runner column (`lifecycle.rs:1320-1332`).
4. `remove_volumes` returns the Volumes it deleted. The gate reads `Ok(Unknown)`, latches `Over(Lost)`, and skips `fill_ingress` (`store_runner.rs:855-861`).
5. `executed` sends the Success, naming the deleted Volumes (`:862`, `:1352-1358`). The Deployment is Unknown and the outcome not Interrupted, so `running` refuses it (`lifecycle.rs:1641-1644`, `mod.rs:1808-1812`). The run deleted Volumes, so it sends nothing again, and `execute` returns the refusal (`store_runner.rs:1349-1351`, `1359-1363`).

No `standing()` read fails on this path. The runner cannot keep this outcome alone. A resend as Interrupted would be kept, but the store reads an Interrupted outcome's deleted Volumes NotAttempted (`lifecycle.rs:2186-2190`), so it would say the Volumes still exist. The refusal at least claims nothing.

On the CLI, the refusal makes the run's command say the Deployment was replaced. On Cloud, `runStoreDeployment` catches the conflict as `{ nothingToRun }` (`dashboard/src/modules/config-store/store-deployment.server.ts:107`). The function then marks the Volumes done, skips `retire-old` (`dashboard/src/modules/config-store/store-deployment.inngest.ts:85`), and skips `mail-failure`, which runs only for a `ran` result that failed or is unknown (`:105-107`). `onFailure`, which calls `stopStoreDeploymentRun` (`:44-50`), never runs. A run that sent its outcome again as Interrupted returns `Ok` with the Deployment Unknown instead, so Cloud reads `ran` and mails the failure.

Closing it needs the store change `store_fixed()` models. `record` keeps a non-Interrupted `Executed` for an Unknown Deployment as Interrupted, in the same transaction, where the code refuses it (`lifecycle.rs:1641-1644`). `removed_volume` shows an Interrupted outcome's deleted Volumes, where the code reads them NotAttempted for anything but a Success (`lifecycle.rs:2186-2190`). Each part has a `Broken` flag and a test that breaks the property it keeps on `store_fixed()`:

| Part of the store fix | `Broken` flag | Test | Property it keeps |
|---|---|---|---|
| `record` keeps a late `Executed` as Interrupted | `store_refuses_late_executed` | `the_store_fix_needs_record_to_keep_a_late_executed` | `TAKEOVER_KEEPS_OUTCOME` |
| The projection shows an Interrupted outcome's deletions | `projection_hides_interrupted_deletions` | `the_store_fix_needs_the_projection_to_show_interrupted_deletions` | `DELETIONS_KEPT` |

When the store change lands, model it as the code writes it, make `store_fix` the default, and turn `the_code_still_loses_the_volumes_a_run_deleted_once_taken_over` into a `Broken` flag that restores the old store, as the four tests under What `Standing` added did for the runner.

## Read a counterexample

A failure prints the test name, the property, the model's flags, and a numbered story cut at the first state in BFS order that breaks the property, so the last row is the broken state. For a `sometimes`, the story is cut at the first state that reaches it (`first_break`). `breaks` keeps that first path with a visitor and stops at the first discovery (`finish_when(HasDiscoveries::AllOf(..))`). `breaks_where` keeps the first one that also passes a filter on the path, and explores the whole model. Each row is one step in the code's words, followed by the Deployment's row and the runner once it ran, for example:

```
 8. Runner: executed records its outcome, naming the deleted Volumes; the Deployment is Unknown and the outcome not Interrupted: running() refuses it; the run deleted Volumes, so it sends nothing again; executed returns that Err (store_runner.rs executed, lifecycle.rs record)
    store: status=Unknown preview=true outcome=None shows_removed=false | runner: Returned last_read=Unknown removed=true standing=Lost
```

`status` is the Deployment's status. `Ended` stands for Applied, Failed or Cancelled. `preview` is whether `record(Prepared)` kept the Deploy Preview. `outcome` is what the store kept, `None` until a record ends the run. `shows_removed` is whether `removed_volume` reads the deleted Volumes Removed. `runner` is the phase in `run_deployment_expecting` and `execute`. A `renewing` phase carries whether its abort ran. `last_read` is what the last `standing()` call that read the store, or `record(Prepared)`'s answer, said. `removed` is whether `remove_volumes` returned. `standing` is the latch `Run::over`: `Keep` until a read latches `Lost` or `Ended`. A row says `(under Broken)` where a flag skipped a guard, and `(the store fix)` where the store fix acted.

Write the story out in the code's words before deciding anything: which reads the runner made, what each said, and where the store's step landed between them. A test that fails inside `assert_properties` prints the raw path of actions instead. To see the rows, call the module's `breaks` helper with that model in a scratch test inside `runner_standing::tests`, since `breaks` is private there.

If a picture would help, the pstack `/statechart` skill draws how a model moves between states and tells each shortest failing path in plain words. Refer to it by name, since its commands change between releases.

## Real bug or model error

A counterexample is a **lead**, not a verdict. Confirm each step of the story against the function it names before calling it a bug. Each `Action` and `Branch` variant's doc comment names its file and function, and `reads_as` prints the statement each step copies.

- If a step does something the code cannot, fix the model and say so. Three places are the usual suspects. `actions` decides which steps are offered in each phase. `RunnerStanding::step` holds what each site does with its read. `RunnerStanding::record` and `RunnerStanding::executed` hold `executed`'s resend, `record`'s `Executed` arm and the projection.
- If every step matches the code, it is a finding. Report it in plain words with the path. Keep the property and the model as they are, because weakening either to go green hides the bug.

What the model assumes, so you can judge the lead. A takeover needs a newer Deployment of the same Environment claimed while this run holds it, which `claim` takes from Queued (`lifecycle.rs:1266-1271`, `1320-1332`). The model gives each read and each piece of work its own step, so a store step can land between any two. Neither finding needs a failed read.

### Staging the open path in real code

The `deploy_client` tests in `core/crates/ployz/tests/deploy_client/` run the real runner and store against a fake Machine, `DeployService` (`support.rs`). The `ployz` helper in `operate.rs` runs the CLI against it (`operate.rs:42`).

1. The first Deploy creates the Volume. The fake lists the Docker Volumes in its `volumes` (`support.rs:103`, `996-1004`), so add one for it there.
2. The second Deploy removes the Volume under destructive acceptance, and it is the run you take over. Its first admission is refused `confirmation_required`. That refusal's `details.version` is the review version bound to the loss digest (`core/crates/ployz-store/src/removal/review.rs:178-181`, `196`). Admit again with `accept_volume_loss` naming the Volume and that bound version. The plain review version is refused `confirmation_required` again, since only the bound one confirms (`:178-181`). A stale-review Conflict carries the bound version in `details.volume_loss.version` (`core/crates/ployz-store/src/review/mod.rs:645-660`, called at `core/crates/ployz-store/src/deployment/admit.rs:388`), not in `details.diff.version`.
3. The fake's `remove_volume` (`support.rs:1148`) has no hold, and the fake's only hold is `hold_health` (`support.rs:338`). Add a hold to `remove_volume`, modelled on it, so the second Deploy's `remove_volumes` waits.
4. While it waits, admit and claim a third Deploy (`ConfigStore::write_trusted` and `claim`, `core/crates/ployz-store/src/lib.rs:225`, `360`). The second Deploy's saved consent covers the loss. Admission passes it into the destructive review (`deployment/admit.rs:408-410`), which accepts a covered loss without a fresh confirmation (`removal/review.rs:172-177`). So the third Deploy needs no `accept_volume_loss` and no version. It still needs the volume observation in `Trusted.volumes` (`core/crates/ployz-store/src/trusted.rs:31-34`), since the review refuses `unavailable` without one (`removal/review.rs:201-215`).
5. Release the hold. The run then takes steps 4 and 5 under What is still open.

Without the Volume, the run sends its refused outcome again as Interrupted. `store_runner_tests.rs` `a_run_taken_over_after_its_last_read_keeps_its_outcome_as_interrupted` shows that against the real store, and `a_lost_run_that_tried_to_delete_a_volume_keeps_the_refusal` shows each removal outcome's answer. `tests/deploy_client/runner_cancellation.rs` `taken_over_mid_create` (`:120-164`) shows a takeover while the first run still executes, which the read after executing records as Interrupted.

## A break that stays green

Guards overlap, so removing one can leave every property holding. Before calling a guard dead, rerun with a neighbouring `Broken` flag set too, or on `store_fixed()`.

- On `as_was()`, `TAKEOVER_KEEPS_OUTCOME`, `DELETIONS_KEPT` and `NO_WORK_AFTER_END` already break, so dropping one runner guard there adds at most `HEEDS_CANCEL`. That is why the guard tests run on `as_is()`.
- On `as_is()`, `TAKEOVER_KEEPS_OUTCOME` and `DELETIONS_KEPT` already break after a deletion, so a guard that keeps them only on that path shows nothing there. `no_resend` shows through the path with nothing deleted, which `the_code_needs_to_send_a_refused_outcome_again` picks with `breaks_where`.
- Dropping the read after executing (`check_ignores_lost`) keeps `TAKEOVER_KEEPS_OUTCOME` on the paths with no deletion, because the resend keeps the outcome as Interrupted. Only `NO_WORK_AFTER_END` breaks, because `remove_volumes` starts after a read said Unknown.
- Dropping the store's takeover refusal (`store_takes_any_outcome`) makes `TAKEOVER_KEEPS_OUTCOME` and `DELETIONS_KEPT` hold, since nothing refuses and the kept Success shows its deletions. The refusal is still right: it keeps a taken-over run's outcome Interrupted (`TAKEN_OVER_INTERRUPTED`).

## Keep the model in step

When a change intentionally alters how the runner reads its standing or how the store answers, such as a new site, a site that acts on another `Standing`, a new store step that sets the status, a reordered record, or a projection that shows deletions differently, change the model in the same PR.

1. Update the model function that copies the product function you changed, citing the file and function it copies, and the `reads_as` line that prints it:
   - `connecting` (`store_runner.rs`): the `Poll` arm of `RunnerStanding::step`.
   - `renewing`: `RunnerStanding::tick` and `Phase::abort`.
   - `execute`: the `Connected`, `Prepare`, `Executed`, `SessionEnds`, `Check`, `Removed`, `Gate`, `Filled` and `Record` arms of `RunnerStanding::step`, and `RunnerStanding::start`.
   - `Run::standing`, `Run::over`, the free fn `standing` and `store_call`: `RunnerStanding::read`, with `Standing`, `Said`, `Read` and `Call`.
   - `executed` and `interrupted`: `RunnerStanding::record`.
   - `record`'s Alive arm (`lifecycle.rs`): `RunnerStanding::read`. Its Executed arm and `removed_volume`: `RunnerStanding::executed`. Its Abandoned arm: `RunnerStanding::abandoned`. Its NotExecuted arm and `finish`/`end`: `finish` and `RunnerStanding::not_executed`. The Prepared arm's `running`: the `Prepare` arm of `step`.
   - `running` (`mod.rs`): `running`.
   - `claim`'s takeover, `cancel` and `cancel_in_flight` (`lifecycle.rs`): the `Takeover`, `Cancel` and `CancelInFlight` arms of `step`, and when `actions` offers each.
2. If the change adds a guard, add a `Broken` flag that removes it and a `breaks` test naming the property it keeps. A guard with no failing variant is untested.
3. If the change ships the store fix, follow What is still open. Do not delete the finding test, since its printed path is the record of what the fix keeps out.
4. If a property's meaning changes, change its text and say why in the PR body. Narrow a property only when the code is right and the property was wrong about the design.
5. If the change adds a branch, add a `COVERAGE` row naming it, with the `omit` that takes it out and the `reach` that says which models have it.
6. Move the sync commit below forward only after you confirmed each covered function still reads as the model says.

## Bounds and blind spots

These are the bounds. There is one Deployment and one runner. The Deploy deletes Volumes when it succeeds. Each store step happens at most once, since each moves the status out of the set that offers it. At most one `standing()` call fails. Executing ends in a Success, a Failed outcome or a session that ended. An aborted piece of work always stops: an aborted prepare records `NotExecuted`, and an aborted execution ends Failed or with its session ended.

These are out of scope:

- a takeover or `cancel_in_flight` between a read and the record the model merges with it, such as `connecting`'s `Cancel` read and its `NotExecuted` (`store_runner.rs:202`), which the store then refuses
- `build`'s own records (`Build`, `Built`), the Deploy's warnings (`record_warnings`, `:848`), and what `fill_ingress` records about ingress (`:886-907`)
- the lease lapsing, which every read remaps to Unknown (`mod.rs:992-994`, `1291-1301`), after `LEASE` (600 s, `mod.rs:344`) with no record
- a second claim by the same runner after a preview, which also sets the Deployment Unknown and keeps its runner (`lifecycle.rs:1241-1252`)
- Cloud's `abandonDeployment` while the run is still going, and the Abandoned arm's `NotExecuted` for a run with no preview (`lifecycle.rs:1909-1922`)
- a dial that fails, a refused prepare, or a build that needs an upload
- a store call other than `standing()` failing, such as `record(Prepared)`, which the run returns as it is (`store_runner.rs:823`), or the resend, which returns the first refusal (`:1361`)
- an abort that comes too late to stop its work
- the select order in `connecting` (`biased`, `:1267`) and `renewing`
- Applied State: it still lists a deleted Volume under the store fix, since a run that was taken over moves no Applied State (`lifecycle.rs:1789-1799`), and `DELETIONS_KEPT` checks only the node projection

A green model says nothing about them. Cover them with `cargo nextest run -p ployz sdk::store_runner` and `cargo nextest run -p ployz --test deploy_client runner_cancellation`.

## Coverage

Each model step is a copy of code at the sync commit. Reread these when a diff touches them:

- `core/crates/ployz/src/sdk/store_runner.rs`: `run_deployment_expecting` from the claim to `execute`. All of `execute`: the plan or build and prepare, `record(Prepared)` read through `standing`, executing under `renewing`, the read after executing, `remove_volumes` with its no-op abort, the gate, `executed` and `Abandoned`. Also `fill_ingress`'s `renewing`, `prepare`'s and `build`'s `renewing`, `connecting`, `renewing`, `Standing`, `Over`, the free fn `standing`, `Run::standing`, `Run::over`, `executed`, `interrupted`, `not_executed`, and `CANCEL_POLL`.
- `core/crates/ployz/src/sdk/store_call.rs`: `store_call`, `bounded` and `unavailable`.
- `core/crates/ployz-store/src/deployment/lifecycle.rs`: `claim`'s re-claim and takeover. `cancel` and `cancel_in_flight`. `record`'s owner check, lease renewal, and its Alive, Prepared, Executed (every refusal, the `taken_over` refusal and keep), NotExecuted and Abandoned arms. Also `finish`, `end`, and the `removed_volume` projection.
- `core/crates/ployz-store/src/deployment/mod.rs`: `running`, `ended`, `owned_elsewhere`, `lapsed`, and the remap to Unknown that `stored` reads through `Standing::status`.
- `core/crates/ployz-core/src/config/runtime_outcome.rs`: the Interrupted arm of the projection.
- `core/crates/ployz-store/src/teardown.rs`: `forget_cluster`'s call to `cancel_in_flight`.

Sync commit: `8307a523654f3c99eee0685a4e7f790088a375e7`, the `main` this model was checked against. Every line cited above is at it, except the `store_runner.rs` lines. Those are at the commit that last changed this file (`git log -1 -- .claude/skills/verify-runner-standing/SKILL.md`), which added `Standing` and the resend; `store_runner_tests.rs` names are at the same commit. Code in the list above that changed after these has not been rechecked.
