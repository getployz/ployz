---
name: verify-build-handoff
description: Model-check a Git build handed to a GitHub Actions run, between Cloud's walk that follows the run (github-open, github-wait and github-check steps, checkStoreGithubBuild's start limit, budget and ended looks, github_finish, cancelStoreGithubBuilds) and the runner's check-in, Build Steps reports and push, with the Deployment runner's report on the build, against a user cancelling the Deployment, GitHub ending the run, an unreachable or refusing grant Machine, a grant Machine that loses the grant, and Inngest running a step again, with the Stateright model in core/crates/ployz-verify/src/build_handoff.rs. Use when a change touches dashboard/src/modules/config-store/store-github-builds.server.ts (startStoreGithubBuild, followPushRun, checkStoreGithubBuild, storeGithubBuildOpen, checkInStoreGithubBuild, grantRuleBuild, recordStoreGithubBuildSteps), walkGithub in dashboard/src/modules/config-store/store-deployment.inngest.ts, cancelStoreGithubBuilds, cancelDispatchedGithubRun or heldGithubBuild in dashboard/src/modules/config-store/config-store.server.ts, core/crates/ployz/src/sdk/github_build.rs (github_check_in, mint, github_rule_grant, rule_build, github_report, github_finish, settle, inspect_grant, grant_read, github_cancel), core/crates/ployz-decide/src/github_build.rs (GithubLook, first, after_read, github_end_holds, github_check_in_holds, Why), or core/crates/ployz-store/src/build/github.rs (record, Stage, GithubHold, building_on, github_dispatched, github_build, github_authorize, github_check_in, github_report, github_end, github_outstanding).
---

# Verify the GitHub build hand-off

The model in `core/crates/ployz-verify/src/build_handoff.rs` explores every interleaving of the processes that read and write one Git build's `config_build` row while a GitHub Actions run holds it. A bare `:n` below is a line of the file named just before it in the same parentheses, and otherwise of `dashboard/src/modules/config-store/store-github-builds.server.ts`. Every path is written in full from the repo root.

## What the model copies

**The actors.** Each is its own process, so one can step between any two of another's Store calls.
- **Cloud's walk.** Cloud's worker runs the Inngest function `run-store-deployment`. Its `walkGithub` (`dashboard/src/modules/config-store/store-deployment.inngest.ts:155-172`) runs, per look, a `github-open` step (`storeGithubBuildOpen`, `:270-272`), a `github-wait` (`step.waitForEvent`), and a `github-check` step that runs `checkStoreGithubBuild` (`:232-267`). Its `store.githubFinish` (`:251`) calls the Store through the native SDK (`core/crates/ployz-sdk/src/config_store.rs:741`) in the worker's own process, so `github_finish`, `settle` and `inspect_grant` (`core/crates/ployz/src/sdk/github_build.rs:710-799`, `core/crates/ployz/src/sdk/github_build.rs:801-828`) run there too. `inspect_grant` dials the grant's Machine from there.
- **The runner.** A GitHub Actions job. Its check-in and its Build Steps reports are HTTP requests Cloud web serves (`:317-332`, `:340-363`). Each calls the Store through the native SDK in Cloud web's process (`core/crates/ployz-sdk/src/config_store.rs:595`, `core/crates/ployz-sdk/src/config_store.rs:712`), which runs `github_check_in` (`core/crates/ployz/src/sdk/github_build.rs:215-318`) or `github_report` (`core/crates/ployz/src/sdk/github_build.rs:645-689`). After a final report, the handler runs its own `githubFinish("ended")` (`:351-356`) and sends the run's event (`:357`). The job's own code, the `getployz/build` Action, is not in this repo. A rule build's job, on the run its push started, checks in through two other requests: `checkInRuleBuild` (`:392-408`), whose `github_rule_check_in` only reads, and `grantRuleBuild` (`:416-435`), whose `heldGithubBuild` (`:422-423`) and `github_rule_grant` (`core/crates/ployz/src/sdk/github_build.rs:403-483`) take the same steps as `github_check_in`: `rule_build`'s read of a building row with no grant (`core/crates/ployz/src/sdk/github_build.rs:487-525`, `:507`), the mint, and the Store's `github_check_in` (`core/crates/ployz/src/sdk/github_build.rs:474`). Its mint never ends the build when it fails. The model's check-in stands for both, with the mint answering `Minted` or `Fails`.
- **A user** cancelling the Deployment. Cloud web commits the Store's `deployment::cancel` (`core/crates/ployz-store/src/deployment/lifecycle.rs`), and `afterWrite` then runs `cancelStoreGithubBuilds` (`dashboard/src/modules/config-store/config-store.server.ts:318-322`).
- **GitHub.** It ends the run once its job exits, or after Cloud's cancel (`cancelGithubRun`, `dashboard/src/modules/github/github-build.server.ts:117-120`), and sends the `workflow_run` webhook, which sends the run's event (`dashboard/src/routes/api/github/-webhook.handler.ts:78-83`).
- **The Deployment runner** on the Deployment's Servers. Its `run-deployment` step runs once every walk returned (`dashboard/src/modules/config-store/store-deployment.inngest.ts:65`, `:80`), and reports the builds it runs through the Store's `deployment::record::record` (`core/crates/ployz-store/src/deployment/record.rs`).
- **The grant's Machine.** It admits one push on a Build Grant: once a tagged manifest is stored, the grant admits no other (`core/crates/ployzd/src/management/build_grant.rs:314`, `:545-551`). It keeps the grant, marked with what it received, in memory until `GRANT_LIFETIME` passes or the daemon stops, and `inspect` reads the push until then (`core/crates/ployzd/src/management/build_grant.rs:6-9`, `:59`, `:265`, `:284-296`). The model lets it lose the grant the row records, once, at any step while the build is on GitHub (`Who::Machine`). After that it admits no push (`core/crates/ployzd/src/management/build_grant.rs:306-315`), and its `inspect` answers NotFound whatever was pushed (`core/crates/ployzd/src/management/build_grant.rs:280-292`), which `grant_read` reads as `Pushed(None)` (`core/crates/ployz/src/sdk/github_build.rs:846-847`).
- **The Store,** Postgres. Each write is one transaction that first takes the Deployment's Environment row lock (`ployz_store::deployment::locked`; `ployz_store::scope::lock_all`) and then reads the build. Writers run read committed and readers in one snapshot (`ployz_store::storage::pg::begin`), so a write sees every write committed before it took the lock, and a read sees one state. Each model write and each model read is one atomic step.

**Every writer of the row's GitHub fields** (`status`, `github`, `github_skipped`, and `message`, the skip reason on a pending row), and what the model does with it:
- `github_dispatched` (`core/crates/ployz-store/src/build/github.rs`), from `recordStoreGithubBuild`, which the start step runs after asking GitHub for each build and before asking for the next (`startStoreGithubBuilds`), and which a record step of its own runs again when the Store refused it (`startGithubBuilds` in `dashboard/src/modules/config-store/store-deployment.inngest.ts`). Modeled for the retried record (`BuildHandoff::hand_off`). The model starts from two states (`init_states`). On the first, `githubDispatched` committed and answered, and the walk's first look is next. On the other, the start's call timed out, so its record runs in a step of its own, which never asks GitHub again and keeps the run. The timed-out call keeps running (`core/crates/ployz/src/sdk/store_call.rs:35-57`), but the Store refuses a record not ready to commit within `LANDS_WITHIN` of being sent, and its commit ends before the caller stops waiting (`ConfigStore::github_dispatched`'s `until`, `core/crates/ployz-store/src/lib.rs`). So a record whose call answered `unavailable` can commit only as the connection drops, before the record step reads the Store. The model's write (`Who::Delayed`) still commits or is refused at any step after, a superset of what the code allows. The record step's steps are `Walk::Start`: its own `githubDispatched` of the same run, and `heldRun`'s second read after a refusal. `State::retried` marks it. Both writes hand the build to the one `RUN`, so the walk follows it whichever commits first. The checker explores the first state first, so a story starts there wherever both reach a state. A rule build follows the run its push started (`:158-168`) instead of dispatching one, and its record is the same write of that run (A rule build, under Bounds and blind spots).
- `github_start`'s reuse and skip ends (`core/crates/ployz/src/sdk/github_build.rs:140-148`) and `githubSkip` (`:127-130`, `core/crates/ployz-sdk/src/config_store.rs:578-581`). Both are `github_end` with no run, which commits only for a waiting build GitHub never passed on (`core/crates/ployz-store/src/build/github.rs`), as the hand-off does (`core/crates/ployz-store/src/build/github.rs`). A first try's skip or end runs before dispatch: left out. A retry's skip or end refused since its read goes to the same second read as its refused hand-off (`:129`, `:138`, `:201`), so the model's refused hand-off stands for it. A retry's skip or end that commits is left out: the walk returns with the build passed on or reused, and the first try's late hand-off is refused.
- `github_check_in` (`core/crates/ployz-store/src/build/github.rs`), from the runner's check-in. Modeled: `BuildHandoff::check_in`'s `Committing` arm.
- `github_end` with `Unstarted`, from the check-in itself: the old-mint skip (`core/crates/ployz/src/sdk/github_build.rs:253-264`), and an enforcing Machine's refusal of the mint, which ends the build with the Machine's sentence and ignores whether that end was refused (`core/crates/ployz/src/sdk/github_build.rs:265-283`). Cloud mints a build it dispatched with no rule as an Operator, which an enforcing Machine refuses (`mint_build_grant` in `core/crates/ployzd/src/machine_api/local.rs`). Modeled: `check_in`'s `Unstarted` arm, with `Why::OldMint` or, after `Mint::Enforced`, `Why::Refused`.
- `github_report` (`core/crates/ployz-store/src/build/github.rs`). Modeled for the final report only (the job's `Reporting` arm). A batch without an end changes only `received` and the log (`core/crates/ployz-store/src/build/github.rs`), which nothing that decides the hand-off reads.
- `github_end` from `github_finish`'s `settle` (`core/crates/ployz/src/sdk/github_build.rs:788-799`), from the walk (`:256`, `:259`, `:261`, `:265`) and from the report handler (`:354`). Modeled: `BuildHandoff::finish`, run by the walk and by the job.
- `github_end` Failed "cancelled" from `github_cancel` (`core/crates/ployz/src/sdk/github_build.rs:867-884`), from the walk (`:241`, `:246`) and from a user's cancel (`dashboard/src/modules/config-store/config-store.server.ts:318-322`). Modeled: `BuildHandoff::stop`, run by the walk at `:241` and by the user. The `:246` call needs an installation the Organization can't use, which the model leaves out (Bounds).
- `record()` (`core/crates/ployz-store/src/build/mod.rs`), from the runner's `run-deployment` step. Modeled: `BuildHandoff::servers`, run by `Who::Servers`. It refuses a build a GitHub run holds or ended (`core/crates/ployz-store/src/build/mod.rs`). In Cloud the step runs only after every walk returned (`dashboard/src/modules/config-store/store-deployment.inngest.ts:65`, `:80`), and the function runs one at a time per Environment (`dashboard/src/modules/config-store/store-deployment.inngest.ts:46`). The model lets the runner report once, after the walk returned (`Walk::Done`), while the build is pending or building.
- `build::settle`, from the Deployment's `save` once the Deployment is out of flight and shows an end. Left out: it writes `status`, `message`, `interrupted` and `finished`, and only on a `building` row with `github` NULL (`core/crates/ployz-store/src/build/github.rs`), one the Servers hold and no GitHub run holds or ended.
- `stopStoreDeploymentRun`'s `cancelStoreGithubBuilds` (`dashboard/src/modules/config-store/store-deployment.server.ts:208`), from the function's `onFailure` and `cancel-store-deployment` (`dashboard/src/modules/config-store/store-deployment.inngest.ts:47-54`, `:136-141`). Left out: it runs once the function failed or was cancelled, after the walk stopped. The model's walk ends `GaveUp` there.

**Every reader that decides from those fields:** `storeGithubBuildOpen` (`:270-272`); `checkStoreGithubBuild`'s `heldGithubBuild` (`:236-237`); `github_finish`'s re-read (`core/crates/ployz/src/sdk/github_build.rs:717-728`); `github_outstanding` (`core/crates/ployz-store/src/build/github.rs`); the check-in's `heldGithubBuild` (`:322-323`) and `github_authorize` with the SDK's pre-check (`core/crates/ployz/src/sdk/github_build.rs:223-232`); and `planStoreGithubBuilds` (`:86`), which runs before dispatch and is left out.

**What each step sees, and who can change it between steps.**
- The walk's read at `:236` and the Deployment read at `:239` are separate reads, so a check-in, a report, a cancel or a user's cancel can land between them, and after `:239`. The model takes `:239`, `installationRefusal` (`:244`) and the clock's budget compare (`:254`) as one step (`At::Deployment`): `installationRefusal` and the clock read nothing the model holds.
- `runEnded` (`:219-221`) is its own step: GitHub can end the run before it or after it.
- `deployDialOf` (`:250`) reads Cloud's connections, which the model does not hold, so it adds no step. `github_finish`'s re-read, `inspect_grant` and the settle (`core/crates/ployz/src/sdk/github_build.rs:717`, `:738`, `:788-799`) are three steps.
- The finding's window runs from the `:236` read to `github_finish`'s re-read (`core/crates/ployz/src/sdk/github_build.rs:717`). Inside it run the Deployment read (`:239`), `installationRefusal` (`:244`), `runEnded` (`:259`) and `deployDialOf` (`:250`). The model has no clock, so it says nothing about how long the window is.
- The check-in's `heldGithubBuild` (`:322-323`) and its pre-check (`core/crates/ployz/src/sdk/github_build.rs:223-232`) are one step. A build `heldGithubBuild` reads as passed on has `github_skipped` set, and `github_dispatched` refuses such a build (`core/crates/ployz-store/src/build/github.rs`), so no write between the two reads makes the second pass where the first failed. Then the mint (`core/crates/ployz/src/sdk/github_build.rs:250`) and the Store's `github_check_in` (`core/crates/ployz/src/sdk/github_build.rs:291-295`) are a step each. `github_input` (`core/crates/ployz/src/sdk/github_build.rs:234-238`) reads only the build's input.
- `github_report`'s `github_authorize` and `github_report` run in one `store_call` closure (`core/crates/ployz/src/sdk/github_build.rs:683-687`) as two transactions. The model takes them as one step: of `github_authorize`'s refusals, only a build with no `github` can follow from a write (`core/crates/ployz-store/src/build/github.rs`; the claims are the run's own), and `github_report` refuses that build too (`core/crates/ployz-store/src/build/github.rs`).
- `cancelStoreGithubBuilds` is three steps: `github_outstanding`'s read, its `github_end`, and the run's cancel.

**Inngest runs a step at least once.** The function has `retries: 2` (`dashboard/src/modules/config-store/store-deployment.inngest.ts:44`), per step. A `github-check` step can die after an effect landed, or fail when a Store call times out after its work committed (`core/crates/ployz/src/sdk/store_call.rs:45-52`); Inngest then runs it again from `:236` with the same `seen`. The model offers that crash (`Pick::Crash`) inside any `github-check` step, once per run by default. A settle `github_end` refuses that is not Unstarted fails the step too, since `settle` returns the conflict and `storeTry` fails with it (`core/crates/ployz/src/sdk/github_build.rs:788-799`). After two re-runs the function fails (`Walk::GaveUp`).
- A re-run of the start limit's step reopens the finding's window: its `:236` read comes after the crash, when Inngest runs the step again (`retries: 2`, `dashboard/src/modules/config-store/store-deployment.inngest.ts:44`). The model offers the crash at every statement after `:236`, so it explores a re-run's window as it does the first (row "a github-check step dies after an effect landed, and Inngest runs it again").
- A re-run of the start limit's step reaches a new state: once its settle committed, the re-run's `:236` read finds the build passed on and returns done, so the run is never cancelled. The row "a re-run of a github-check step finds the build no longer building" reaches it. That run cannot check in afterwards: its pre-check finds the build passed on.
- A re-run of `github-open` repeats a read, the same as running it later. A wait is not re-run.
- A re-run of the record step after the start's `githubDispatched` timed out (`core/crates/ployz/src/sdk/store_call.rs:35-57`) runs while that write may still commit. It writes the same run again; GitHub is never asked again. If the first write commits first, `github_dispatched` refuses the second (`core/crates/ployz-store/src/build/github.rs`), and `heldRun`'s second read finds the run holding the build and follows it. If the second commits first, the first write is refused, and the walk follows the run. The second read can also find no run holding it: the run already ended (its old-mint skip, or a user's cancel) or the Deployment is no longer in flight. Cloud then cancels the run and the step answers `done`, as the walk would for that run. The run can check in before the start limit's look, and the start limit waits for it and a later look settles it, or never, and a look passes it on. The rows "after a retried record, …" and "the first try's hand-off …" reach each. A re-run of a start step whose answer Inngest lost reads each build first, and the step recorded each build before asking for the next, so it follows every run it asked for: the first state. `start_leaves_held_run` drops the second read (The findings).

## The findings

Three findings, each fixed in the code. Each has a `Broken` flag that puts it back, and its tests keep the path it took.

**The start limit settled by the grant.** Before `GithubLook`, the start limit's look read the build with `build.grant === null` (`:236`, `:258`) and, with the Deployment queued and the run running, called `finish(true)`: the `timed_out` the budget passed too. Inside that window the runner's check-in could commit: its pre-check passes, the Machine mints a grant, and the Store's `github_check_in` records the grant and its time (`core/crates/ployz-store/src/build/github.rs`). `github_finish(timed_out=true)` re-read the build (`core/crates/ployz/src/sdk/github_build.rs:717`) and found the grant. `inspect_grant` read nothing pushed (a fresh grant, `Pushed(None)`), so the end was the budget's Skipped "the run ran out of time" (now `core/crates/ployz/src/sdk/github_build.rs:750-752`). `github_end` accepts that from the run that holds the build (`core/crates/ployz-store/src/build/github.rs`): the Unstarted grant guard (`core/crates/ployz-store/src/build/github.rs`) applies only to Unstarted. The build went back to pending with `github_skipped` set (`core/crates/ployz-store/src/build/github.rs`), `finish` answered done, and Cloud cancelled the run (`:262`), which already held its Build Grant and build secrets. With the grant's Machine unreachable, the budget's Failed "your Machine couldn't be reached to confirm the push" failed the build instead (now `core/crates/ployz/src/sdk/github_build.rs:743-749`). `start_limit_reads_the_grant` puts this back, and the tests' `timed_out()` sets it.

`checkStoreGithubBuild`'s comment allowed the cancel: "GitHub keeps one that started", where it now reads "GitHub keeps one whose check-in committed" (`:229-230`). The Build Order rule says two things (`dashboard/CONTEXT.md:214`). A build moves to the next Builder "only when the current one does not start the build in time", and "A build that has started moves on only when GitHub fails it for infrastructure reasons (the runner stopped before its final report, the run pushed nothing without a failed Build Step, the runner couldn't install ployz, or the run ran out of time)". On GitHub Actions, running out of time is the 2-hour budget: "On GitHub Actions it has 2 hours, and if it runs out of time there, the next builder in your build order takes it" (`docs/user/builds/overview.md:56-57`; `GITHUB_RUN_BUDGET_MS`, `dashboard/src/modules/config-store/store-github-builds.server.ts:36`, counted from the check-in at `dashboard/src/modules/config-store/store-github-builds.server.ts:254`). The start limit wrote that budget's message for a run that checked in after the start limit ran out (`START_WITHIN_MINUTES`, `dashboard/src/modules/config-store/store-github-builds.server.ts:40`; `dashboard/src/modules/config-store/store-deployment.inngest.ts:162-165`), with its budget just begun. And the code answered "in time" two ways. A check-in that commits one step later, after `github_finish`'s re-read and before its settle, was already kept: the Unstarted settle is refused, and `github_finish` answers Waiting, "It checked in meanwhile: the next look finds it started" (`core/crates/ployz/src/sdk/github_build.rs:781-785`). So whether the start limit kept a run that checked in turned on which side of the re-read its check-in committed. Neither sentence of the rule says which side of the start limit a check-in that commits while the start limit settles is on. The property and the code read "in time" as that Waiting does: a run started in time when its check-in committed before the start limit settled. That is this model's reading of the rule, not the rule's words.

The build itself went on: Skipped put it back to pending, and `startLimit` needs a next Builder (`dashboard/src/modules/config-store/store-deployment.inngest.ts:162`), the Servers, which the runner's `run-deployment` step builds on (`targets` in `core/crates/ployz/src/sdk/store_runner.rs`). What was lost is the GitHub run that already started.

**A Server could report on a build a GitHub run holds.** `record()` checked no holder, so a runner's report on a build a GitHub run held or ended wrote its status, message and Server over the run's (`core/crates/ployz-store/src/build/mod.rs`). It now refuses one (`core/crates/ployz-store/src/build/mod.rs`), and the Store reads a `building` row as held by the run or by the Servers, never both (`Stage` and `By`, `core/crates/ployz-store/src/build/mod.rs`). `record_ignores_github` drops the guard.

**A retried start left the Servers a build its run held.** Found after `record`'s guard was added, in two rounds. When `github-start`'s first try timed out on `githubDispatched`, Cloud cancelled its run (`:195`), and Inngest ran the step again. The re-run dispatched a second run, which `github_dispatched` refused once the first try's write had committed (`core/crates/ployz-store/src/build/github.rs`). Cloud cancelled that run too and answered `done`, so `walkGithub` returned (`dashboard/src/modules/config-store/store-deployment.inngest.ts:159`) while the first run still held the build, and `run-deployment` sent the building source to the Servers (`targets`, `core/crates/ployz/src/sdk/store_runner.rs`). Before the guard, `record` took their report over the run's hold. With the guard, it refuses their first Building report (`Run::build` and `Run::follow` in `core/crates/ployz/src/sdk/store_runner.rs`). The runner stops, Cloud reads the conflict as nothing to run (`dashboard/src/modules/config-store/store-deployment.server.ts:129`), and the Deployment lapses Unknown once its runner lease runs out (`core/crates/ployz-store/src/deployment/mod.rs:1475`). A first read that follows a held build (`:121-122`) covers a first try's write that committed before the re-run, but not one that commits after that read. After any refused write, its hand-off, skip or end, the re-run now reads the build again and follows the run that holds it (`refused`, `:125`): those writers refuse only under the Environment lock, so that read sees the winner. A re-run that answers `done` without that read has just committed its own skip or end, which refuses the first try's late hand-off. `start_leaves_held_run` answers `done` after a refused hand-off instead. Since the record moved to a step of its own, a retry no longer dispatches a run at all: it writes the run the first try asked for, and after a refused write `recordStoreGithubBuild` reads the build again and follows the run that holds it.

**The properties.** `STARTED_RUN_KEPT`, an `always`: "the start limit never ends a run whose check-in committed, nor cancels one while it holds the build". `State::limit_took` is set when the walk's start-limit `github_end` commits on a run whose grant is recorded, any end, or when its `:262` cancel lands while the run still holds the build with its grant. The cancel clause covers only a run that still holds the build. Once another actor ended the hold, that actor's end decided the run, not the start limit: the run's own final report and `github_finish`, after which the run has nothing left to do, or a user's cancel, whose `cancelStoreGithubBuilds` cancels the run itself. The model has one run, so the flag watches every run whose check-in committed.

`NO_SERVER_ON_GITHUB_BUILD`, an `always`: "no Server's report lands on a build a GitHub run holds". `State::server_on_github` is set when `record` takes the Servers' report on a row whose `github` is set.

`SERVERS_REPORT_LANDS`, an `always`: "record never refuses the Servers' report on a build the walk left them". `State::servers_refused` is set when `record` refuses it, which stops the runner.

## Run it

```
cd core && cargo nextest run -p ployz-verify build_handoff --no-capture
```

Expected: the summary line ends with `18 tests run: 18 passed, 338 skipped`. `the_code_keeps_a_started_run` prints `the code: 1033499 states`, `timed_out_breaks_only_the_finding` prints `timed out: 1116898 states`, and `the_check_in_guard_refuses_a_second_grant` prints `two requests: 2723909 states`. Each story prints under its test's name. A warm run took 100 s of nextest time at `-j6`, on a shared machine. CI runs `ployz-verify` in its workspace nextest run.

Read the tests in three groups.
- **The finding, with the start limit settling by the grant** (`timed_out()`, which sets `start_limit_reads_the_grant`).
  - `timed_out_passes_on_a_run_that_checked_in` replays the finding's twelve actions with `follow`, through the cancel, and asserts the start limit read no grant before the check-in committed, the Skipped settle, the pending row and the cancel. It then asks the checker for its first broken path (`breaks`) and asserts the same race ends it.
  - `timed_out_fails_a_run_that_checked_in_when_its_machine_is_unreachable` keeps to broken paths whose last step is the Failed "unreachable" settle.
  - `timed_out_breaks_only_the_finding` expects `STARTED_RUN_KEPT` to break, `NO_GRANT_AFTER_CANCEL`, `ONE_GRANT_PER_RUN`, `NO_SERVER_ON_GITHUB_BUILD` and `SERVERS_REPORT_LANDS` to hold, and every `COVERAGE` row to be reached.
  - `timed_out_needs_the_unstarted_guard` also sets `unstarted_ignores_grant`, and keeps to broken paths whose last step is an Unstarted settle: `github_finish`'s re-read found no grant, and a check-in committed after it.
- **The code.**
  - `the_code_keeps_a_started_run` runs `code()` with `assert_properties`: every `always` holds, and every row that `on` keeps for `code()` is reached.
  - `the_start_limit_waits_for_the_finding_s_run` replays the finding's schedule on the code: the start limit's Unstarted is refused, `github_finish` answers Waiting, Cloud does not cancel, and the walk opens its next look.
  - `a_later_look_ends_the_run_the_start_limit_waited_for` asks for the first path to each of the rows "after the start limit waited for a run that checked in, an Ended look settles it" and "… the budget settles it".
  - `a_retried_start_follows_the_held_run` asks for the first path, from the retried record, on which the start limit ends the run Unstarted and the Servers' report lands.
  - `a_retried_start_follows_a_hand_off_that_commits_after_its_read` asks for the first path to the retry's second read following the run: the first try's write commits, and the retry's write of the same run is refused.
  - `a_retried_start_s_run_that_checked_in_keeps_its_build` asks for the first path on which the run of a retried record checks in before the start limit's look: the start limit's Unstarted is refused, and a later look settles the run.
  - `the_start_limit_on_each_edge_state` runs the start limit's `github_finish` on each state its re-read can find: no grant (settles Unstarted), a grant without an end and a grant with its end reported (Waiting, row unchanged), passed on, built and failed (each returned, row unchanged). For another run's build, it runs `github_end`'s Unstarted for `RUN` on a row run 1 holds: it is refused and the row is unchanged. `github_finish` takes the run from its re-read (`core/crates/ployz/src/sdk/github_build.rs:729`), so only `github_end` is ever given a run that may not hold the build.
- **The code's guards.** Each sets one flag on `code_without(..)` (The code's guards): `the_start_limit_needs_its_look`, `the_start_limit_needs_the_unstarted_guard`, `the_start_limit_needs_the_done_gate`, `the_check_in_guard_refuses_a_cancelled_deployment`, `the_check_in_guard_refuses_a_second_grant`, `a_retried_start_needs_its_second_read` and `record_needs_its_github_guard`. The second check-in test runs two check-in requests of one run, first with the guard (every `always` holds), then without it. `a_retried_start_needs_its_second_read` keeps to the path on which the first try's write commits between the retry's read and its hand-off. The walk leaves the Servers no build a run holds, so `record_needs_its_github_guard` first checks that `record_ignores_github` alone breaks nothing, then sets `start_leaves_held_run` too. It also runs `record` with its guard on the start state, where the run holds the build (refused, row unchanged), and on a build GitHub passed on (taken, Building).

## The code's guards

`github_finish` takes the look that calls it (`GithubLook` in `core/crates/ployz-decide/src/github_build.rs`), and the start limit issues only Unstarted, whatever the re-read found (`core/crates/ployz/src/sdk/github_build.rs:730-736, :781-785`).

```text
github_finish("start_limit")   re-read the build; passed on or not building: return it;
                               else settle GithubEnd::Unstarted "no runner started the
                               build in time", whatever the grant. github_end's grant guard
                               refuses it once a check-in committed, and github_finish
                               answers Waiting (core/crates/ployz/src/sdk/github_build.rs:781-785)
github_finish("budget")        as timed_out=true did: an unreachable Machine fails the build,
                               and nothing pushed is Skipped "the run ran out of time"
github_finish("ended")         as timed_out=false did: an unreachable Machine answers Waiting
```

Cloud's walk keeps its control flow, and passes the look (`:251`, `:256`, `:259`, `:261`, `:265`, `:354`): on Waiting the start limit returns waiting (`:263`), so it does not cancel (`:262`), and the walk opens its next look with `GITHUB_CHECK_INTERVAL` (`dashboard/src/modules/config-store/store-deployment.inngest.ts:160-166`). That look ends the run through the Ended look once the run reports or completes (`:265`), or through the budget (`:254-256`).

| Guard | What it keeps | Flag | Test, and the `always` property that breaks without it |
|---|---|---|---|
| The start limit issues only Unstarted (`core/crates/ployz/src/sdk/github_build.rs:730-736, :781-785`) | A run whose check-in committed inside the window: the start limit's end is refused, not settled | `start_limit_reads_the_grant` | `the_start_limit_needs_its_look`, `STARTED_RUN_KEPT` |
| The Unstarted grant guard (`core/crates/ployz-store/src/build/github.rs`) | The same run: without it the start limit's Unstarted passes the run on | `unstarted_ignores_grant` | `the_start_limit_needs_the_unstarted_guard`, `STARTED_RUN_KEPT`; with `start_limit_reads_the_grant`, `timed_out_needs_the_unstarted_guard` |
| The done-gate on Cloud's cancel (`:262`) | The same run: on Waiting, Cloud does not cancel it | `cancel_ignores_done` | `the_start_limit_needs_the_done_gate`, `STARTED_RUN_KEPT` |
| The check-in's guard (`github_check_in_holds`, called by `ployz_store::build::github_check_in`) | No check-in for a cancelled Deployment, and no second grant over the first | `check_in_unguarded` | `the_check_in_guard_refuses_a_cancelled_deployment`, `NO_GRANT_AFTER_CANCEL`; `the_check_in_guard_refuses_a_second_grant`, `ONE_GRANT_PER_RUN`, with two requests |
| After a refused write, the retried record reads the build again and follows the run that holds it (`recordStoreGithubBuild`) | The walk returns only once no run holds the build, so the Servers' report lands | `start_leaves_held_run` | `a_retried_start_needs_its_second_read`, `SERVERS_REPORT_LANDS` |
| `record`'s GitHub guard (`core/crates/ployz-store/src/build/github.rs`) | No Server's report on a build a GitHub run holds or ended | `record_ignores_github` | `record_needs_its_github_guard`, `NO_SERVER_ON_GITHUB_BUILD`, with `start_leaves_held_run` |

What the code keeps, and what it leaves:
- **A run that checked in and never reports waits for GitHub or the budget.** The next look's `:265` waits until `runEnded` says it completed, or the budget ends it. The rows "after the start limit waited for a run that checked in, an Ended look settles it", "… the budget settles it" and "… the report's own github_finish settles it" reach each end on the code and under `timed_out()`. Under `timed_out()` they are reached by the check-in landing after `github_finish`'s re-read, the path `github_finish`'s Waiting (`core/crates/ployz/src/sdk/github_build.rs:781-785`) was written for.
- **The start limit still cancels a run another actor already ended.** When the run's own report and `github_finish` settled it, or `github_cancel` failed it, inside the window, the start limit's re-read returns it and Cloud cancels the run (`:262`). The run has already sent its final report, or the user's cancel cancels it anyway.
- **One unreachable read still fails a build under the budget** (`core/crates/ployz/src/sdk/github_build.rs:743-749`). That is the budget's choice, not the start limit's.

## Read a counterexample

A story prints when a test finds its path, under `--no-capture`: `breaks` and `breaks_where` print the path they find, under the test name, the property and the model's flags, and `follow` prints the schedule it replays, with "a schedule" in place of the property. A story is numbered and cut at the first state in BFS order that breaks the property (for a `sometimes`, the first that reaches it), so its last row is that state. `breaks` keeps that first path with a single-thread visitor and stops at the first discovery. `breaks_where` keeps the first path that also passes a filter on the path, and explores the whole model. One thread keeps the path the same from run to run.

Some tests print a story before they fail, and some print none. To print the story for a failing property, keep the edit that turned the test red, add a scratch test to the `tests` module naming the model and property that broke, and run it alone:

```rust
#[test]
fn scratch() {
    breaks(code(), STARTED_RUN_KEPT);
}
```

```
cd core && cargo nextest run -p ployz-verify build_handoff::tests::scratch --no-capture
```

`reads_as` writes each row's words. A model edit that changes what a step does needs its `reads_as` arm changed with it, or the story describes the old step. A row nothing reaches has no story: the checker found no state for it.

These are rows 3 to 11 of `timed_out_passes_on_a_run_that_checked_in`'s second story, the checker's first broken path under `start_limit_reads_the_grant`:

```
 3. Cloud walk: github-check-0: heldGithubBuild reads it building, with build.grant === null (:236)
    row: building, github run 0, no grant | Deployment: Queued | run: running | walk: Checking { check: 0, ended: false, attempt: 0, at: Deployment { grant: false, ended: false } } | job: Queued
 4. Cloud walk: github-check: the Deployment is queued (:239), and the installation is usable (:244-248); runEnded is next (:259)
    row: building, github run 0, no grant | Deployment: Queued | run: running | walk: Checking { check: 0, ended: false, attempt: 0, at: AskingGithub { grant: false } } | job: Queued
 5. Cloud walk: github-check: runEnded says the run is still running (:219-221); at the start limit it calls finish("start_limit") (:261)
    row: building, github run 0, no grant | Deployment: Queued | run: running | walk: Checking { check: 0, ended: false, attempt: 0, at: Finishing { look: StartLimit, at: Read } } | job: Queued
 6. Runner: check-in: heldGithubBuild and github_authorize read it building with no grant, so the pre-check passes (:322-323, core/crates/ployz/src/sdk/github_build.rs:223-232)
    row: building, github run 0, no grant | Deployment: Queued | run: running | walk: Checking { check: 0, ended: false, attempt: 0, at: Finishing { look: StartLimit, at: Read } } | job: CheckingIn(Minting)
 7. Runner: check-in: the Machine mints a Build Grant (core/crates/ployz/src/sdk/github_build.rs:250)
    row: building, github run 0, no grant | Deployment: Queued | run: running | walk: Checking { check: 0, ended: false, attempt: 0, at: Finishing { look: StartLimit, at: Read } } | job: CheckingIn(Committing)
 8. Runner: check-in: the Store's github_check_in finds the run holding the build, in flight with no grant, and sets the grant and checked_in_at (core/crates/ployz-store/src/build/github.rs); the runner gets its grant and build secrets (core/crates/ployz/src/sdk/github_build.rs:299-317)
    row: building, github run 0, grant | Deployment: Queued | run: running | walk: Checking { check: 0, ended: false, attempt: 0, at: Finishing { look: StartLimit, at: Read } } | job: Building
 9. Cloud walk: github_finish("start_limit"), settling by the grant as timed_out=true did (under Broken): re-reads the build and finds the grant (core/crates/ployz/src/sdk/github_build.rs:717, :737); inspect_grant is next (core/crates/ployz/src/sdk/github_build.rs:738)
    row: building, github run 0, grant | Deployment: Queued | run: running | walk: Checking { check: 0, ended: false, attempt: 0, at: Finishing { look: StartLimit, at: Inspect { ended: None } } } | job: Building
10. Cloud walk: github_finish("start_limit"), settling by the grant as timed_out=true did (under Broken): inspect_grant reads the grant on its Machine: Pushed(None) (core/crates/ployz/src/sdk/github_build.rs:801-828, :846-847)
    row: building, github run 0, grant | Deployment: Queued | run: running | walk: Checking { check: 0, ended: false, attempt: 0, at: Finishing { look: StartLimit, at: Settle(Skipped(RanOutOfTime)) } } | job: Building
11. Cloud walk: github_finish("start_limit"), settling by the grant as timed_out=true did (under Broken): settle: github_end accepts Skipped "the run ran out of time" from the run that holds the build (core/crates/ployz-store/src/build/github.rs), and writes it pending (core/crates/ployz-store/src/build/github.rs)
    row: pending, github none, github_skipped | Deployment: Queued | run: running | walk: Checking { check: 0, ended: false, attempt: 0, at: LimitCancel { waiting: false } } | job: Building
```

Each row is one step, named for who takes it: Cloud walk, Runner, Runner's second check-in (with two requests), Deployment runner, User, GitHub, Grant's Machine or First try's hand-off. A bare `:n` in a row follows the same rule as above. The line under it is the state after the step:
- `row` is the build's `config_build` row: its status, its `github` (run, grant, the final report's end) and whether `github_skipped` is set. "grant" is the job's request's grant, "second grant" the second request's.
- `Deployment` is queued or cancelled. `run` is whether GitHub says the run completed, whether Cloud sent its cancel, whether the runner pushed, and whether the grant's Machine lost the grant ("grant lost").
- `walk` and `job` are each actor's next step: `Walk::Checking`'s `at` names the `checkStoreGithubBuild` statement next, and `attempt` counts the step's runs before this one.

A row that skips a guard says "(under Broken)" when a flag is set.

A test that fails inside `assert_properties` (`the_code_keeps_a_started_run`) reports only the first property it finds broken, and prints the raw actions instead of the rows. An action is `Action { who, pick }`: `who` is `Walk`, `Job`, `Again`, `Servers`, `User`, `Github`, `Machine` or `Delayed`, and `pick` is `Go` or the choice the step made (`PastBudget`, `GithubDown`, `Grant(..)`, `Mint(..)`, `Push`, `ReportBuilt`, `ReportFailed`, `Quit`, `Crash`). To see the rows, call `breaks` with that model and property in a scratch test.

## Real bug or model error

A counterexample is a **lead**, not a verdict. Before calling it a bug, confirm each step of the story against the line it cites. Each `At`, `Finish`, `Job`, `Branch` and `Why` variant's doc comment names its lines, and `reads_as` prints the statement each step copies.

- If a step does something the code cannot, fix the model and say so. The usual suspects:
  - `offers`, which decides what each actor can do in each state;
  - `BuildHandoff::check`, for `checkStoreGithubBuild`'s ladder;
  - `BuildHandoff::finish`, for `github_finish` and `settle`;
  - `State::github_end`, `check_in`, `pre_check` and the job's `Reporting` arm, for the Store's guards;
  - `BuildHandoff::servers`, for `record`'s guard.
- If every step matches the code, it is a finding. Keep the property and the model as they are, since weakening either to go green hides the bug.

## Breaking a guard yourself

Set its `Broken` flag through `code_without`. That changes one model, so only the tests that set the flag see it. Add `--no-fail-fast` to the nextest command, so every red test is listed.

Each flag breaks only the `always` property its test names, and each test fails without its flag. To break a guard that has no flag, edit the model. Each break below was applied alone (B21's two edits together) to `core/crates/ployz-verify/src/build_handoff.rs`, or to `core/crates/ployz-decide/src/github_build.rs`, the real code the model calls, where the break says so, at this commit, run with `cd core && cargo nextest run -p ployz-verify build_handoff --no-fail-fast`, and reverted, and turned exactly the tests it names red. B6 to B9, B22 and B24 make a flag do nothing, so the tests that need it go red; `timed_out()` sets `start_limit_reads_the_grant`, so B9 turns its tests red too.

**B1. a timed-out look answers Waiting where it settles Skipped "the run ran out of time".** Red: `the_code_keeps_a_started_run`, `timed_out_breaks_only_the_finding`, `timed_out_passes_on_a_run_that_checked_in`.

In `after_read` (`core/crates/ployz-decide/src/github_build.rs`), which the model calls:

```diff
-_ if look == GithubLook::Budget => Ending::Skipped(Why::RanOutOfTime),
+_ if look == GithubLook::Budget => return None,
```

**B2. a timed-out look reads Unreachable as Waiting.** Red: `the_code_keeps_a_started_run`, `timed_out_breaks_only_the_finding`, `timed_out_fails_a_run_that_checked_in_when_its_machine_is_unreachable`.

In `after_read` (`core/crates/ployz-decide/src/github_build.rs`):

```diff
-GrantRead::Unreachable if !look.timed_out() => return None,
+GrantRead::Unreachable if true => return None,
```

**B3. the property's settle clause dropped.** Red: `the_start_limit_needs_its_look`, `the_start_limit_needs_the_unstarted_guard`, `timed_out_breaks_only_the_finding`, `timed_out_fails_a_run_that_checked_in_when_its_machine_is_unreachable`, `timed_out_needs_the_unstarted_guard`, `timed_out_passes_on_a_run_that_checked_in`.

```diff
-                        if limit && started {
-                            state.limit_took = true;
-                        }
```

**B4. the property's cancel clause dropped.** Red: `the_start_limit_needs_the_done_gate`.

```diff
-                if state.row.started_on(RUN) {
-                    state.limit_took = true;
-                }
```

**B5. the settle clause's `started` is always false.** Red: `a_later_look_ends_the_run_the_start_limit_waited_for`, `a_retried_start_s_run_that_checked_in_keeps_its_build`, `the_code_keeps_a_started_run`, `the_start_limit_needs_its_look`, `the_start_limit_needs_the_unstarted_guard`, `the_start_limit_waits_for_the_finding_s_run`, `timed_out_breaks_only_the_finding`, `timed_out_fails_a_run_that_checked_in_when_its_machine_is_unreachable`, `timed_out_needs_the_unstarted_guard`, `timed_out_passes_on_a_run_that_checked_in`.

```diff
-let started = state.row.started_on(RUN);
+let started = false;
```

**B6. unstarted_ignores_grant made a no-op.** Red: `the_start_limit_needs_the_unstarted_guard`, `timed_out_needs_the_unstarted_guard`.

```diff
-End::Unstarted(_) if broken.unstarted_ignores_grant => EndKind::Failed,
+End::Unstarted(_) if false => EndKind::Failed,
```

**B7. check_in_unguarded made a no-op.** Red: `the_check_in_guard_refuses_a_cancelled_deployment`, `the_check_in_guard_refuses_a_second_grant`.

```diff
-if !self.broken.check_in_unguarded
-    && !github_check_in_holds(state.row.hold(RUN), wanted)
+if !github_check_in_holds(state.row.hold(RUN), wanted)
```

**B8. cancel_ignores_done made a no-op.** Red: `the_start_limit_needs_the_done_gate`.

```diff
-(!waiting || self.broken.cancel_ignores_done)
+!waiting
```

**B9. start_limit_reads_the_grant made a no-op.** Red: `the_start_limit_needs_its_look`, `timed_out_breaks_only_the_finding`, `timed_out_fails_a_run_that_checked_in_when_its_machine_is_unreachable`, `timed_out_passes_on_a_run_that_checked_in`.

```diff
-let called = if look == GithubLook::StartLimit && self.broken.start_limit_reads_the_grant {
+let called = if false {
```

**B10. the start limit's look gone.** Red: `the_check_in_guard_refuses_a_second_grant`, `the_code_keeps_a_started_run`, `the_start_limit_on_each_edge_state`, `the_start_limit_waits_for_the_finding_s_run`.

In `first` (`core/crates/ployz-decide/src/github_build.rs`): every look reads the grant once the run checked in.

```diff
-Some(grant) if look != GithubLook::StartLimit => First::ReadGrant(grant),
+Some(grant) => First::ReadGrant(grant),
```

**B11. the check-in ignores the Deployment.** Red: `the_check_in_guard_refuses_a_second_grant`, `the_code_keeps_a_started_run`, `timed_out_breaks_only_the_finding`.

In `github_check_in_holds` (`core/crates/ployz-decide/src/github_build.rs`):

```diff
-building_on == Some(Hold::Dispatched) && in_flight
+building_on == Some(Hold::Dispatched)
```

**B12. the check-in ignores a grant.** Red: `the_check_in_guard_refuses_a_second_grant`.

In `github_check_in_holds` (`core/crates/ployz-decide/src/github_build.rs`):

```diff
-building_on == Some(Hold::Dispatched) && in_flight
+building_on.is_some() && in_flight
```

**B13. wake does nothing.** Red: `the_code_keeps_a_started_run`, `timed_out_breaks_only_the_finding`.

In `State::wake`, after `} = self.walk`:

```diff
-        {
+            && false
+        {
```

**B14. Inngest never retries.** Red: `the_code_keeps_a_started_run`, `timed_out_breaks_only_the_finding`.

```diff
-let retried = attempt < RETRIES;
+let retried = false;
```

**B15. the pre-check ignores the grant.** Red: none (see below).

```diff
-.is_some_and(|github| github.grant.is_none());
+.is_some();
```

**B16. runEnded always running.** Red: `the_code_keeps_a_started_run`, `timed_out_breaks_only_the_finding`.

```diff
-let says_ended = state.completed && !down;
+let says_ended = false && !down;
```

**B17. the start limit also cancels when finish answers Waiting.** Red: `the_check_in_guard_refuses_a_second_grant`, `the_code_keeps_a_started_run`, `the_start_limit_waits_for_the_finding_s_run`, `timed_out_passes_on_a_run_that_checked_in`.

```diff
-(!waiting || self.broken.cancel_ignores_done)
+(true || self.broken.cancel_ignores_done)
```

**B18. the grant's Machine never loses a grant.** Red: `the_code_keeps_a_started_run`, `timed_out_breaks_only_the_finding`.

```diff
-if granted && !state.lost {
+if false && granted && !state.lost {
```

**B19. inspect reads a lost grant's push.** Red: `the_code_keeps_a_started_run`, `timed_out_breaks_only_the_finding`.

```diff
-let read_push = state.pushed && !state.lost;
+let read_push = state.pushed;
```

**B20. one look.** Red: `a_later_look_ends_the_run_the_start_limit_waited_for`, `a_retried_start_s_run_that_checked_in_keeps_its_build`, `the_code_keeps_a_started_run`, `timed_out_breaks_only_the_finding`.

```diff
-const MAX_CHECKS: u8 = 2;
+const MAX_CHECKS: u8 = 1;
```

**B21. the start limit settles by the grant once the run checked in.** Two edits, applied together: the first makes the start limit settle by the grant whenever the re-read finds one, and the second keeps that step's `reads_as` in step. Red: `the_check_in_guard_refuses_a_second_grant`, `the_code_keeps_a_started_run`, `the_start_limit_on_each_edge_state`, `the_start_limit_waits_for_the_finding_s_run`, the same set as B10.

The first edit, in `first` (`core/crates/ployz-decide/src/github_build.rs`), is B10's: `first` settles Unstarted for a run with no grant at every look, so dropping the start limit's look and settling by the grant once the run checked in are one change there.

```diff
-Some(grant) if look != GithubLook::StartLimit => First::ReadGrant(grant),
+Some(grant) => First::ReadGrant(grant),
```

```diff
-if look == GithubLook::StartLimit && by == Finisher::Walk && !model.broken.start_limit_reads_the_grant {
+if false && look == GithubLook::StartLimit && by == Finisher::Walk && !model.broken.start_limit_reads_the_grant {
```

**B22. record_ignores_github made a no-op.** Red: `record_needs_its_github_guard`.

```diff
-let ok = !github || self.broken.record_ignores_github;
+let ok = !github;
```

**B23. record takes every report.** Red: `a_retried_start_needs_its_second_read`, `record_needs_its_github_guard`.

```diff
-let ok = !github || self.broken.record_ignores_github;
+let ok = true;
```

**B24. start_leaves_held_run made a no-op.** Red: `a_retried_start_needs_its_second_read`, `record_needs_its_github_guard`.

```diff
-} else if self.broken.start_leaves_held_run {
+} else if false {
```

**B25. the Servers report before the walk returned.** Red: `a_retried_start_needs_its_second_read`, `record_needs_its_github_guard`, `the_check_in_guard_refuses_a_second_grant`, `the_code_keeps_a_started_run`, `timed_out_breaks_only_the_finding`.

```diff
-                    && state.walk == Walk::Done
```

B15 turns nothing red: the Store's own guard at `core/crates/ployz-store/src/build/github.rs` refuses the same check-in, so no property needs the pre-check's grant half. It is the code's, so the model keeps it.

To make one `COVERAGE` row unreachable, put `false && ` inside its closure, before the body. A one-line predicate becomes `reached: |_, state| false && took(state, …)`; a block predicate becomes `reached: |_, state| false && { … }`. Each of the 74 rows of the model before the record step was made unreachable that way, alone, and turned these tests red. The record step dropped the row for the retry's first read and renamed the three below; that sweep was not run again for the 73 rows:
- the other 67 rows: red `the_code_keeps_a_started_run`, `timed_out_breaks_only_the_finding`.
- "the start limit settles a run that checked in Skipped "the run ran out of time""; "the start limit fails a run that checked in, its grant's Machine unreachable": red `timed_out_breaks_only_the_finding`.
- "after the start limit waited for a run that checked in, an Ended look settles it"; "after the start limit waited for a run that checked in, the budget settles it": red `a_later_look_ends_the_run_the_start_limit_waited_for`, `the_code_keeps_a_started_run`, `timed_out_breaks_only_the_finding`.
- "the first try's hand-off commits first: the retry's write is refused, and its second read follows the run": red `a_retried_start_follows_a_hand_off_that_commits_after_its_read`, `the_code_keeps_a_started_run`, `timed_out_breaks_only_the_finding`.
- "after a retried record, a look passes on the run, and the Servers take the build": red `a_retried_start_follows_the_held_run`, `the_code_keeps_a_started_run`, `timed_out_breaks_only_the_finding`.
- "after a retried record, the run checked in: the start limit waits for it, and a later look settles it": red `a_retried_start_s_run_that_checked_in_keeps_its_build`, `the_code_keeps_a_started_run`, `timed_out_breaks_only_the_finding`.

## Bounds and blind spots

The bounds, and why each leaves out no new state:
- **One run, one check-in request by default.** A run's check-in is the one request the model's job sends. `the_check_in_guard_refuses_a_second_grant` runs two (`requests: 2`), for the guard's grant half. The Action's code is not in this repo, so whether it retries a check-in is not shown here. A retried record never dispatches a second run: it writes the run the start asked for.
- **One late write.** Only the start's write can commit late (`Who::Delayed`), and the model's record step answers. The code narrows this: the Store refuses a record past its deadline (`LANDS_WITHIN`, 15s from the call, checked after every statement but the commit), and the caller waits 30s, more than the deadline plus the database's 10s statement bound on the commit. So once a record's call answered, the record never lands later, and no lost answer can make its run the holder after Cloud moved on to a replay's run or a give-up. The exception is a connection lost while committing, whose commit settles at once. Once the step's retries run out, the walk never hands the build to the Servers. The function's `onFailure` stops only runs the Store recorded (`dashboard/src/modules/config-store/store-deployment.inngest.ts`, `onFailure`), so on its last try the record step gives up a run Cloud dispatched (`giveUpRun`, `dashboard/src/modules/config-store/store-github-builds.server.ts`). Against a connection lost while committing, it first passes the build on (`githubSkip`), which `github_dispatched` refuses to follow with any run (`core/crates/ployz-store/src/build/github.rs`), and cancels the run only once that pass-on commits, or is refused while the run isn't the holder. If the Store doesn't answer, the run is left alone. It holds the build only if its record committed as the connection dropped, and then failure cleanup cancels it once the Store answers. A push's own run is never cancelled. The Deployment, still unclaimed, fails (`dashboard/src/modules/config-store/store-deployment.server.ts:153-160`, `:208-211`). Against trunk's per-build start, which cancelled its dispatched run whenever its record failed, even a record that committed: this never cancels the run that holds the build, and leaves two runs trunk cancelled running until their check-in is refused, both tracked in #4695. One is a give-up the Store never answers. The other is a start step whose record the Store refused and whose answer Inngest then lost: its retry asks GitHub again, and no step holds the first run's id. Trunk's timed-out record could also land after its retry dispatched again, which the deadline now refuses.
- **Two looks** (`MAX_CHECKS`). The finding is at check 0, and check 1 is the later look that ends a run the start limit waited for. With one look (B20 above), the rows "after the start limit waited for a run that checked in, …" are never reached. Lifted to four looks in the scratch lift below, which finds nothing new.
- **One crash by default,** within Inngest's `retries: 2`. Lifted to two crashes in the scratch lift below. `the_check_in_guard_refuses_a_second_grant` runs with no crash.
- **No clock.** The budget (`:254`) can be past whenever the run checked in, and every wait can time out or take an event. That leaves out timing, not orders. A grant's expiry is the clock's too: the model takes it, with a daemon restart, as the grant's Machine losing the grant, at any step. The model says nothing about how long the window is or how often it opens.
- **A next Builder.** The model's walk has one, so check 0 is the start limit (`dashboard/src/modules/config-store/store-deployment.inngest.ts:162`). Without one, `startLimit` is false at every check, and `:260` returns waiting. With one, no look after check 0 reads a run with no grant (the start limit ended it, or its Unstarted was refused for a check-in), so the model has no row for `:260`.
- **An installation the Organization can use** (`:244-248`). Losing it fails the Deployment (`:245-247`) through `cancelStoreGithubBuilds`, which the model runs at `:241`; `stopGithubInstallationBuilds` cancels the run and wakes the walk (`dashboard/src/modules/config-store/config-store.server.ts:79-103`). Neither is the start limit.
- **GitHub ends the run** only after Cloud's cancel or once the job exited. A job inside a request when GitHub ends it finishes that request first (Cloud web serves it) and exits after. The steps a request takes after the run completed read nothing GitHub holds, so the same states are reached as when GitHub ends the run right after the request.
- **A rule build** runs on the run its push started, which Cloud follows (`followPushRun`, `:158-168`, `:209-217`) rather than dispatches. Its check-in is `grantRuleBuild`'s, whose reads, mint and `github_check_in` are the model's check-in with a mint that answers `Minted` or `Fails` (What the model copies), and its reports, looks and cancels are the dispatched run's. Its start differs in two ways. Both tries of a retried start hand off the same run, which `github_dispatched` takes again without a change, so the walk follows that run whichever write commits first: the state the model's first try reaches, with the run not cancelled. And a run that ended before Cloud followed it is run again first (`rerunGithubRun`, `:163`), before any hand-off, so no look sees its first end. A retry whose skip commits leaves the push's run holding nothing: its `grantRuleBuild` finds no build (`ruleBuildOf`, `:372-386`), and it never gets a grant. The model has no state these add, so it has no rule-build mode. Not covered: a `workflow_run` event for a run's first attempt that reaches a wait after Cloud reran it. The wait would read the run as ended (`seen.ended`), and a look would pass on a build whose rerun has yet to check in. That needs GitHub to deliver the first attempt's event after Cloud listed the run as completed.
- **Events.** A wait gets the run's event only while it waits; an event sent at another time is lost, as `step.waitForEvent` loses it.

The lift. Set `const MAX_CHECKS: u8 = 4;`, add this test to the `tests` module, run it in release, and revert both:

```rust
#[test]
fn scratch_lifts() {
    for (name, model) in [
        ("timed out, 2 crashes", BuildHandoff { crashes: 2, ..timed_out() }),
        ("the code, 2 crashes", BuildHandoff { crashes: 2, ..code() }),
        ("timed out, 2 requests, 1 crash", BuildHandoff { requests: 2, ..timed_out() }),
        ("the code, 2 requests, 1 crash", BuildHandoff { requests: 2, ..code() }),
    ] {
        let start = std::time::Instant::now();
        let checker = model.checker().threads(4).spawn_bfs().join();
        let broken: Vec<_> = [
            STARTED_RUN_KEPT,
            NO_GRANT_AFTER_CANCEL,
            ONE_GRANT_PER_RUN,
            NO_SERVER_ON_GITHUB_BUILD,
            SERVERS_REPORT_LANDS,
        ]
            .into_iter()
            .filter(|property| checker.discovery(property).is_some())
            .collect();
        let unreached: Vec<_> = COVERAGE
            .iter()
            .filter(|row| (row.on)(&model))
            .filter(|row| checker.discovery(row.name).is_none())
            .map(|row| row.name)
            .collect();
        println!(
            "LIFT {name}, {MAX_CHECKS} checks: {} states in {:?}; always broken: {broken:?}; rows unreached: {unreached:?}",
            checker.unique_state_count(),
            start.elapsed()
        );
    }
}
```

```
cd core && cargo nextest run -p ployz-verify build_handoff::tests::scratch_lifts --no-capture --cargo-profile release
```

It prints these four lines; the times vary from run to run:

```
LIFT timed out, 2 crashes, 4 checks: 7152180 states in 26.112220391s; always broken: ["the start limit never ends a run whose check-in committed, nor cancels one while it holds the build"]; rows unreached: []
LIFT the code, 2 crashes, 4 checks: 6969655 states in 24.45367391s; always broken: []; rows unreached: []
LIFT timed out, 2 requests, 1 crash, 4 checks: 30097386 states in 109.194455926s; always broken: ["the start limit never ends a run whose check-in committed, nor cancels one while it holds the build"]; rows unreached: []
LIFT the code, 2 requests, 1 crash, 4 checks: 29102464 states in 102.373010877s; always broken: []; rows unreached: []
```

The whole command took 280 s wall in one run, release build included.

Each timed-out model breaks only `STARTED_RUN_KEPT`, each model of the code breaks no `always` property, and every row its model can reach is reached.

Left out:
- **The job's own work.** The Action, between its check-in and its reports, is not in this repo. The model's job pushes, sends a final report of Built (after a push) or Failed, or stops.
- **Batches without an end**, which change only `received` and the log.
- **A Built final report without a push**, which `github_finish` settles Skipped "the run ended without pushing an image"; the model reaches that end through a job that stops.
- **`hasNext` false,** the Deployment running or cancelling during the walk, and a user cancelling twice.
- **What a step costs**, and whether GitHub, a Machine or the Store answers within its timeouts, beyond the crash.

A green model says nothing about them. Cover the code with:

```
cd core && cargo nextest run -p ployz-store --test git github
cd dashboard && pnpm install --frozen-lockfile && pnpm sdk
cd dashboard && pnpm exec vitest run --project postgres src/modules/config-store/store-github-builds.postgres.test.ts
```

Each line starts from the repo root and changes into `core` or `dashboard` itself. Expected: the first's summary line ends with `11 tests run: 11 passed, 23 skipped`. The second installs the dashboard's packages, then `pnpm sdk` runs `core/scripts/build-cloud-sdk.sh` (`dashboard/package.json:27`). That script fetches the native SDK the third loads from the artifact cache worktrees share, keyed by the SDK's sources, and builds it with Cargo only when the cache has no match (`core/scripts/build-cloud-sdk.sh:9`, `:20-23`, `:31-45`); it then runs the SDK's contract and Node tests (`core/scripts/build-cloud-sdk.sh:70-71`). Without it, the third fails with `Cannot find module './ployz-sdk.node'`. The third starts its own Postgres through Docker and prints `Tests  8 passed (8)`.

## Follow-ups

- **A run that pushed and reported Built can be passed on as one that "ended without pushing an image".** This is on main, apart from the start limit.
  - **How.** A Machine forgets every grant when its daemon restarts, and a grant once `GRANT_LIFETIME` passes (`core/crates/ployzd/src/management/build_grant.rs:6-9`, `:59`, `:265`, `:284-292`). Its `inspect` then answers NotFound, "this Machine holds no such Build Grant; it expired or the daemon restarted", even for a grant that received the push (`core/crates/ployzd/src/management/build_grant.rs:280-292`). `grant_read` reads NotFound as `Pushed(None)` (`core/crates/ployz/src/sdk/github_build.rs:846-847`). With the run's Built report recorded, `github_finish` settles Skipped "the run ended without pushing an image" at an Ended look (`core/crates/ployz/src/sdk/github_build.rs:750-752`), or "the run ran out of time" at the budget (`core/crates/ployz/src/sdk/github_build.rs:750-752`), in place of Built (`core/crates/ployz/src/sdk/github_build.rs:753-776`). `github_end` puts the build back to pending with that reason (`core/crates/ployz-store/src/build/github.rs`).
  - **What follows depends on the Build Order.** With the Servers in it, they build it again (`targets` in `core/crates/ployz/src/sdk/store_runner.rs`). Without them, as in a GitHub-only order, the Deployment fails with, for a Service named `web`, "Build failed. web: the run ended without pushing an image. No other Builder in your Build Order can take it" (`targets` in `core/crates/ployz/src/sdk/store_runner.rs`; `then_sentence` in `core/crates/ployz-core/src/error_chain.rs`; `docs/user/troubleshooting/builds.md:110-111`). The model always has a next Builder (A next Builder, under Bounds and blind spots), so its rows cover only the rebuild.
  - **What a user sees.** The build's log gains "GitHub skipped: the run ended without pushing an image" (`core/crates/ployz-store/src/build/github.rs`). `ployz deploy` prints "GitHub skipped it: the run ended without pushing an image" under the build (`core/crates/ployz/src/handlers/deploy.rs:1710-1711`). Its summary reads "GitHub skipped web: the run ended without pushing an image." until the Servers built it, and "web built on <server>. GitHub skipped it: the run ended without pushing an image." after (the `(Some(built), Some(why))` and `(None, Some(why))` arms in `core/crates/ployz/src/handlers/deploy/progress.rs`). The dashboard's build panel shows the build's message, "the run ended without pushing an image", in red above its log, unless the Deployment's failure reason already carries it (`dashboard/src/routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/-components/deployments/RunPanel.tsx:59-61`), until the Servers' report replaces the message (`core/crates/ployz-store/src/build/github.rs`).
  - **Bet 8.** The Machine answered that it holds no such grant, not that nothing was pushed, so "the run ended without pushing an image" is "Certainty the observer lacks" (`core/DESIGN.md:205`).
  - **The model.** The two rows "after the grant's Machine lost a grant that received the push, …" reach the inspect that reads nothing pushed for a Built report, with its Skipped settle next, on the code and under `timed_out()` (`lost_after_built`).
  - **Timing.** The two clocks start at different moments. The grant's 3 hours start at its mint (`core/crates/ployzd/src/management/build_grant.rs:59`, `:260`). The mint finishes before the separate Store call that records the check-in (`core/crates/ployz/src/sdk/github_build.rs:250`, `:291-295`). The Store stamps that call's time as `checked_in_at` (`core/crates/ployz-store/src/build/github.rs`), and Cloud's 2-hour budget counts from that stamp (`dashboard/src/modules/config-store/store-github-builds.server.ts:36`, `:254`). A daemon restart drops the grant whenever it happens (`core/crates/ployzd/src/management/build_grant.rs:6-9`).
  - **Not known.** Losing the grant does not by itself remove the image: the grant's proxy forwards the push to the Machine's image ingest (`core/crates/ployzd/src/management/build_grant.rs:499-512`, `:545-548`), and the grant itself holds only the pushed digest (`core/crates/ployzd/src/management/build_grant.rs:547-548`). Whether the ingest still serves the image after a real restart or expiry, so that the Servers' build could reuse it, was not measured. Nor was how often a daemon restarts between a push and Cloud's look. No issue is filed.

## Keep the model in step

When a change alters a writer or reader of the build row's GitHub fields, or a step of the walk, change the model in the same PR. Examples: a new `github_finish` caller, a new end, a guard on `record()`, a change to `:254-266`'s order.

1. Update the function that copies the product function you changed, and the `reads_as` arm that prints it:
   - `checkStoreGithubBuild`: `BuildHandoff::check` and `At`.
   - `walkGithub` and `storeGithubBuildOpen`: `BuildHandoff::walk` and `Walk`.
   - `github_finish`, `settle` and `inspect_grant`: `BuildHandoff::finish`, `Finish` and `GrantRead`.
   - `github_end`: `State::github_end`.
   - `github_check_in`, `mint`, the old-mint skip and the enforcing Machine's refusal, and `github_rule_grant` and `rule_build`: `BuildHandoff::pre_check`, `check_in`, `CheckIn` and `Mint`.
   - `github_report` and `recordStoreGithubBuildSteps`: the job's `Reporting`, `Finishing` and `Waking` arms.
   - `github_cancel` and `cancelStoreGithubBuilds`: `BuildHandoff::stop` and `Stop`.
   - the user's cancel: `BuildHandoff::user`.
   - `record`: `BuildHandoff::servers` and `Servers`.
   - `startStoreGithubBuilds`' reads, `recordStoreGithubBuild` and the record step in `startGithubBuilds`: `BuildHandoff::start`, `hand_off`, `Start`, `Who::Delayed` and `init_states`'s retried state. Its followed push run, `followPushRun` and `rerunGithubRun`: the "A rule build" bullet under Bounds and blind spots, which argues the dispatched start stands for it; a change that breaks that argument needs the model.
2. If the change adds a guard, add a `Broken` flag that removes it, and a test naming the property it keeps.
3. If the change alters what a look settles, keep `timed_out()` as the start limit before `GithubLook`, and keep its tests, since their printed paths are the record of what the start limit's look keeps out.
4. If the change adds a branch, add a `COVERAGE` row whose predicate requires the branch it names (`took`).
5. Every `State` field is read: `deployment`, `row`, `pushed` (by `finish`'s inspect), `cancelled` and `completed` (by `offers` and `runEnded`), `walk`, `job`, `again`, `user`, `crashes` (by `offers`), `limit_took` (by `STARTED_RUN_KEPT`), `limit_waited` (by the rows), `granted_after_cancel` (by `NO_GRANT_AFTER_CANCEL`), `grant_replaced` (by `ONE_GRANT_PER_RUN`), `retried` (by the rows and `start`), `delayed` (by `offers`), `servers` (by `offers`), `server_on_github` (by `NO_SERVER_ON_GITHUB_BUILD`), `servers_refused` (by `SERVERS_REPORT_LANDS`) and `took` (by the rows). A field that is only written still splits states, so remove one that nothing reads.
6. Move the sync commit below forward only after you confirmed each covered function still reads as the model says.

## Coverage

Each model step is a copy of code at the sync commit. Reread these when a diff touches them:

- `dashboard/src/modules/config-store/store-github-builds.server.ts`: `startStoreGithubBuild`'s follow of a held build, its followed push run (`followPushRun`) and its `githubDispatched` calls, `checkStoreGithubBuild`, `storeGithubBuildOpen`, `runEnded`, `checkInStoreGithubBuild`, `ruleBuildOf`, `grantRuleBuild` and `recordStoreGithubBuildSteps`.
- `dashboard/src/modules/config-store/store-deployment.inngest.ts`: `walkGithub`, the `run-deployment` step's place after the walks, and the function's `retries` and `concurrency`.
- `dashboard/src/modules/config-store/config-store.server.ts`: `cancelDispatchedGithubRun`, `heldGithubBuild`, `cancelStoreGithubBuilds` and `afterWrite`'s cancel.
- `dashboard/src/routes/api/github/-webhook.handler.ts`: the `workflow_run` event.
- `core/crates/ployz/src/sdk/github_build.rs`: `github_check_in`, `mint`, `github_rule_grant`, `rule_build`, `github_report`, `github_finish`, `settle`, `inspect_grant`, `grant_read` and `github_cancel`.
- `core/crates/ployz-decide/src/github_build.rs`: `GithubLook`, `first`, `after_read`, `github_end_holds`, `github_check_in_holds` and `Why`, which the model calls.
- `core/crates/ployz-store/src/build/github.rs`: `record`, `Stage`, `GithubHold`, `building_on`, `github_build`, `github_authorize`, `github_check_in`, `github_report`, `github_end` and `github_outstanding`.
- `core/crates/ployz-store/src/deployment/lifecycle.rs`: `cancel`.
- `core/crates/ployz-store/src/deployment/record.rs`: `record`, the call for a runner's build report.

Sync commit: `48582970f957293b4e37d63c7620a5d620b4a24b`, the commit this model was checked against, and every line cited above is at it. Code in the list above that changed after it has not been rechecked.
