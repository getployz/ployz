---
name: verify-build-handoff
description: Model-check a Git build handed to a GitHub Actions run, between Cloud's walk that follows the run (github-open, github-wait and github-check steps, checkStoreGithubBuild's start limit, budget and ended looks, github_finish, cancelStoreGithubBuilds) and the runner's check-in, Build Steps reports and push, with the Deployment runner's report on the build, against a user cancelling the Deployment, GitHub ending the run, an unreachable or refusing grant Machine, a grant Machine that loses the grant, and Inngest running a step again, with the Stateright model in core/crates/ployz-verify/src/build_handoff.rs. Use when a change touches dashboard/src/modules/config-store/store-github-builds.server.ts (checkStoreGithubBuild, storeGithubBuildOpen, checkInStoreGithubBuild, recordStoreGithubBuildSteps), walkGithub in dashboard/src/modules/config-store/store-deployment.inngest.ts, cancelStoreGithubBuilds, cancelDispatchedGithubRun or heldGithubBuild in dashboard/src/modules/config-store/config-store.server.ts, core/crates/ployz/src/sdk/github_build.rs (github_check_in, mint, github_report, GithubLook, github_finish, settle, inspect_grant, grant_read, github_cancel), or core/crates/ployz-store/src/build.rs (record, Stage, GithubHold, building_on, github_dispatched, github_build, github_authorize, github_check_in, github_report, github_end, github_outstanding).
---

# Verify the GitHub build hand-off

The model in `core/crates/ployz-verify/src/build_handoff.rs` explores every interleaving of the processes that read and write one Git build's `config_build` row while a GitHub Actions run holds it. A bare `:n` below is a line of the file named just before it in the same parentheses, and otherwise of `dashboard/src/modules/config-store/store-github-builds.server.ts`. Every path is written in full from the repo root.

## What the model copies

**The actors.** Each is its own process, so one can step between any two of another's Store calls.
- **Cloud's walk.** Cloud's worker runs the Inngest function `run-store-deployment`. Its `walkGithub` (`dashboard/src/modules/config-store/store-deployment.inngest.ts:151-167`) runs, per look, a `github-open` step (`storeGithubBuildOpen`, `:228-230`), a `github-wait` (`step.waitForEvent`), and a `github-check` step that runs `checkStoreGithubBuild` (`:190-225`). Its `store.githubFinish` (`:209`) calls the Store through the native SDK (`core/crates/ployz-sdk/src/config_store.rs:648`) in the worker's own process, so `github_finish`, `settle` and `inspect_grant` (`core/crates/ployz/src/sdk/github_build.rs:466-569`, `core/crates/ployz/src/sdk/github_build.rs:584-611`) run there too. `inspect_grant` dials the grant's Machine from there.
- **The runner.** A GitHub Actions job. Its check-in and its Build Steps reports are HTTP requests Cloud web serves (`:275-290`, `:298-321`). Each calls the Store through the native SDK in Cloud web's process (`core/crates/ployz-sdk/src/config_store.rs:582`, `core/crates/ployz-sdk/src/config_store.rs:619`), which runs `github_check_in` (`core/crates/ployz/src/sdk/github_build.rs:187-274`) or `github_report` (`core/crates/ployz/src/sdk/github_build.rs:384-432`). After a final report, the handler runs its own `githubFinish("ended")` (`:309-314`) and sends the run's event (`:315`). The job's own code, the `getployz/build` Action, is not in this repo.
- **A user** cancelling the Deployment. Cloud web commits the Store's `cancel` (`core/crates/ployz-store/src/deployment/lifecycle.rs:1485-1503`), and `afterWrite` then runs `cancelStoreGithubBuilds` (`dashboard/src/modules/config-store/config-store.server.ts:306-310`).
- **GitHub.** It ends the run once its job exits, or after Cloud's cancel (`cancelGithubRun`, `dashboard/src/modules/github/github-build.server.ts:86-95`), and sends the `workflow_run` webhook, which sends the run's event (`dashboard/src/routes/api/github/-webhook.handler.ts:78-83`).
- **The Deployment runner** on the Deployment's Servers. Its `run-deployment` step runs once every walk returned (`dashboard/src/modules/config-store/store-deployment.inngest.ts:62`, `:79`), and reports the builds it runs through the Store's `record` (`core/crates/ployz-store/src/deployment/lifecycle.rs:1850`).
- **The grant's Machine.** It admits one push on a Build Grant: once a tagged manifest is stored, the grant admits no other (`core/crates/ployzd/src/management/build_grant.rs:244`, `:470-476`). It keeps the grant, marked with what it received, in memory until `GRANT_LIFETIME` passes or the daemon stops, and `inspect` reads the push until then (`core/crates/ployzd/src/management/build_grant.rs:6-9`, `:49`, `:188`, `:207-226`). The model lets it lose the grant the row records, once, at any step while the build is on GitHub (`Who::Machine`). After that it admits no push (`core/crates/ployzd/src/management/build_grant.rs:237-245`), and its `inspect` answers NotFound whatever was pushed (`core/crates/ployzd/src/management/build_grant.rs:203-221`), which `grant_read` reads as `Pushed(None)` (`core/crates/ployz/src/sdk/github_build.rs:629-630`).
- **The Store,** Postgres. Each write is one transaction that first takes the Deployment's Environment row lock (`deployment::locked`, `core/crates/ployz-store/src/deployment/mod.rs:1604-1608`; `core/crates/ployz-store/src/scope.rs:568-580`) and then reads the build. Writers run read committed and readers in one snapshot (`core/crates/ployz-store/src/storage/pg.rs:107-120`), so a write sees every write committed before it took the lock, and a read sees one state. Each model write and each model read is one atomic step.

**Every writer of the row's GitHub fields** (`status`, `github`, `github_skipped`, and `message`, the skip reason on a pending row), and what the model does with it:
- `github_dispatched` (`core/crates/ployz-store/src/build.rs:1013-1036`), from the `github-start` step (`:164-166`). Modeled for the retried start (`BuildHandoff::hand_off`). The model starts from two `github-start` steps (`init_states`). On the first try, `githubDispatched` committed and answered, and the walk's first look is next (`dashboard/src/modules/config-store/store-deployment.inngest.ts:153-154`). On the retry, the first try's call timed out, so Cloud cancelled its run (`:166`) and Inngest ran the step again. The timed-out call keeps running (`core/crates/ployz/src/sdk/store_call.rs:35-55`), so its write (`Who::Delayed`) commits or is refused at any step after. The retry's steps are `Walk::Start`: `holder`'s read (`:118-119`), its own run's `githubDispatched`, and `holder`'s second read after a refusal (`refused`, `:122`). `State::retried` marks the retry's start. The model keeps one `RUN`: the first try's run, until the retry's hand-off commits, which makes `RUN` the retry's run (`retry_won`). The run that loses never held the build, so its check-in and its late hand-off are refused, and Cloud cancelled it. The checker explores the first try's start first, so a story starts there wherever both reach a state.
- `github_start`'s reuse and skip ends (`core/crates/ployz/src/sdk/github_build.rs:112-120`) and `githubSkip` (`:124-127`, `core/crates/ployz-sdk/src/config_store.rs:565-568`). Both are `github_end` with no run, which commits only for a waiting build GitHub never passed on (`core/crates/ployz-store/src/build.rs:1252`), as the hand-off does (`core/crates/ployz-store/src/build.rs:1023-1028`). A first try's skip or end runs before dispatch: left out. A retry's skip or end refused since its read goes to the same second read as its refused hand-off (`:126`, `:135`, `:172`), so the model's refused hand-off stands for it. A retry's skip or end that commits is left out: the walk returns with the build passed on or reused, and the first try's late hand-off is refused.
- `github_check_in` (`core/crates/ployz-store/src/build.rs:1141-1173`), from the runner's check-in. Modeled: `BuildHandoff::check_in`'s `Committing` arm.
- `github_end` with `Unstarted`, from the SDK's old-mint skip (`core/crates/ployz/src/sdk/github_build.rs:225-235`). Modeled: the `OldMint` arm.
- `github_report` (`core/crates/ployz-store/src/build.rs:1177-1233`). Modeled for the final report only (the job's `Reporting` arm). A batch without an end changes only `received` and the log (`core/crates/ployz-store/src/build.rs:1208-1217`), which nothing that decides the hand-off reads.
- `github_end` from `github_finish`'s `settle` (`core/crates/ployz/src/sdk/github_build.rs:558-569`), from the walk (`:214`, `:217`, `:219`, `:223`) and from the report handler (`:312`). Modeled: `BuildHandoff::finish`, run by the walk and by the job.
- `github_end` Failed "cancelled" from `github_cancel` (`core/crates/ployz/src/sdk/github_build.rs:650-667`), from the walk (`:199`, `:204`) and from a user's cancel (`dashboard/src/modules/config-store/config-store.server.ts:306-310`). Modeled: `BuildHandoff::stop`, run by the walk at `:199` and by the user. The `:204` call needs an installation the Organization can't use, which the model leaves out (Bounds).
- `record()` (`core/crates/ployz-store/src/build.rs:264-314`), from the runner's `run-deployment` step. Modeled: `BuildHandoff::servers`, run by `Who::Servers`. It refuses a build a GitHub run holds or ended (`core/crates/ployz-store/src/build.rs:289-296`). In Cloud the step runs only after every walk returned (`dashboard/src/modules/config-store/store-deployment.inngest.ts:62`, `:79`), and the function runs one at a time per Environment (`dashboard/src/modules/config-store/store-deployment.inngest.ts:43`). The model lets the runner report once, after the walk returned (`Walk::Done`), while the build is pending or building.
- `build::settle`, from the Deployment's `save` once the Deployment is out of flight and shows an end. Left out: it writes `status`, `message`, `interrupted` and `finished`, and only on a `building` row with `github` NULL (`core/crates/ployz-store/src/build.rs:181-188`), one the Servers hold and no GitHub run holds or ended.
- `stopStoreDeploymentRun`'s `cancelStoreGithubBuilds` (`dashboard/src/modules/config-store/store-deployment.server.ts:186`), from the function's `onFailure` and `cancel-store-deployment` (`dashboard/src/modules/config-store/store-deployment.inngest.ts:44-51`, `:135-140`). Left out: it runs once the function failed or was cancelled, after the walk stopped. The model's walk ends `GaveUp` there.

**Every reader that decides from those fields:** `storeGithubBuildOpen` (`:228-230`); `checkStoreGithubBuild`'s `heldGithubBuild` (`:194-195`); `github_finish`'s re-read (`core/crates/ployz/src/sdk/github_build.rs:473-484`); `github_outstanding` (`core/crates/ployz-store/src/build.rs:1303-1317`); the check-in's `heldGithubBuild` (`:280-281`) and `github_authorize` with the SDK's pre-check (`core/crates/ployz/src/sdk/github_build.rs:195-204`); and `planStoreGithubBuilds` (`:83`), which runs before dispatch and is left out.

**What each step sees, and who can change it between steps.**
- The walk's read at `:194` and the Deployment read at `:197` are separate reads, so a check-in, a report, a cancel or a user's cancel can land between them, and after `:197`. The model takes `:197`, `installationRefusal` (`:202`) and the clock's budget compare (`:212`) as one step (`At::Deployment`): `installationRefusal` and the clock read nothing the model holds.
- `runEnded` (`:177-179`) is its own step: GitHub can end the run before it or after it.
- `deployDialOf` (`:208`) reads Cloud's connections, which the model does not hold, so it adds no step. `github_finish`'s re-read, `inspect_grant` and the settle (`core/crates/ployz/src/sdk/github_build.rs:473`, `:506`, `:558-569`) are three steps.
- The finding's window runs from the `:194` read to `github_finish`'s re-read (`core/crates/ployz/src/sdk/github_build.rs:473`). Inside it run the Deployment read (`:197`), `installationRefusal` (`:202`), `runEnded` (`:217`) and `deployDialOf` (`:208`). The model has no clock, so it says nothing about how long the window is.
- The check-in's `heldGithubBuild` (`:280-281`) and its pre-check (`core/crates/ployz/src/sdk/github_build.rs:195-204`) are one step. A build `heldGithubBuild` reads as passed on has `github_skipped` set, and `github_dispatched` refuses such a build (`core/crates/ployz-store/src/build.rs:1023-1028`), so no write between the two reads makes the second pass where the first failed. Then the mint (`core/crates/ployz/src/sdk/github_build.rs:222`) and the Store's `github_check_in` (`core/crates/ployz/src/sdk/github_build.rs:247-251`) are a step each. `github_input` (`core/crates/ployz/src/sdk/github_build.rs:206-210`) reads only the build's input.
- `github_report`'s `github_authorize` and `github_report` run in one `store_call` closure (`core/crates/ployz/src/sdk/github_build.rs:426-430`) as two transactions. The model takes them as one step: of `github_authorize`'s refusals, only a build with no `github` can follow from a write (`core/crates/ployz-store/src/build.rs:1057-1059`; the claims are the run's own), and `github_report` refuses that build too (`core/crates/ployz-store/src/build.rs:1190-1192`).
- `cancelStoreGithubBuilds` is three steps: `github_outstanding`'s read, its `github_end`, and the run's cancel.

**Inngest runs a step at least once.** The function has `retries: 2` (`dashboard/src/modules/config-store/store-deployment.inngest.ts:41`), per step. A `github-check` step can die after an effect landed, or fail when a Store call times out after its work committed (`core/crates/ployz/src/sdk/store_call.rs:45-52`); Inngest then runs it again from `:194` with the same `seen`. The model offers that crash (`Pick::Crash`) inside any `github-check` step, once per run by default. A settle `github_end` refuses that is not Unstarted fails the step too, since `settle` returns the conflict and `storeTry` fails with it (`core/crates/ployz/src/sdk/github_build.rs:558-569`). After two re-runs the function fails (`Walk::GaveUp`).
- A re-run of the start limit's step reopens the finding's window: its `:194` read comes after the crash, when Inngest runs the step again (`retries: 2`, `dashboard/src/modules/config-store/store-deployment.inngest.ts:41`). The model offers the crash at every statement after `:194`, so it explores a re-run's window as it does the first (row "a github-check step dies after an effect landed, and Inngest runs it again").
- A re-run of the start limit's step reaches a new state: once its settle committed, the re-run's `:194` read finds the build passed on and returns done, so the run is never cancelled. The row "a re-run of a github-check step finds the build no longer building" reaches it. That run cannot check in afterwards: its pre-check finds the build passed on.
- A re-run of `github-open` repeats a read, the same as running it later. A wait is not re-run.
- A re-run of `github-start` after its first try's `githubDispatched` timed out (`core/crates/ployz/src/sdk/store_call.rs:35-55`) runs while that write may still commit. `holder`'s first read (`:118-119`) follows a run that already holds the build. Otherwise the retry dispatches its own run. If the first try's write commits before the retry's hand-off, `github_dispatched` refuses the retry's run (`core/crates/ployz-store/src/build.rs:1020-1028`), Cloud cancels it (`:166`), and `holder`'s second read follows the first try's run (`:122`, `:172`). A retry that skips GitHub instead, its skip refused, reads the same way (`:126`). If the retry's hand-off commits first, the first try's write is refused, and the walk follows the retry's run. Either way the walk follows the run that holds the build. The second read can also find no run holding it: the first try's run already ended (its old-mint skip, or a user's cancel) or the Deployment is no longer in flight, and the step answers `done`, as the walk would for that run. GitHub can end the first try's cancelled run before its check-in, and a look passes it on, or after it, and the start limit waits for it and a later look settles it. The rows "after a retried start, …" and "the first try's hand-off …" reach each. `start_leaves_held_run` drops the second read (The findings).

## The findings

Three findings, each fixed in the code. Each has a `Broken` flag that puts it back, and its tests keep the path it took.

**The start limit settled by the grant.** Before `GithubLook`, the start limit's look read the build with `build.grant === null` (`:194`, `:216`) and, with the Deployment queued and the run running, called `finish(true)`: the `timed_out` the budget passed too. Inside that window the runner's check-in could commit: its pre-check passes, the Machine mints a grant, and the Store's `github_check_in` records the grant and its time (`core/crates/ployz-store/src/build.rs:1141-1173`). `github_finish(timed_out=true)` re-read the build (`core/crates/ployz/src/sdk/github_build.rs:473`) and found the grant. `inspect_grant` read nothing pushed (a fresh grant, `Pushed(None)`), so the end was the budget's Skipped "the run ran out of time" (now `core/crates/ployz/src/sdk/github_build.rs:546-548`). `github_end` accepts that from the run that holds the build (`core/crates/ployz-store/src/build.rs:1251`): the Unstarted grant guard (`core/crates/ployz-store/src/build.rs:1248-1250`) applies only to Unstarted. The build went back to pending with `github_skipped` set (`core/crates/ployz-store/src/build.rs:1277-1285`), `finish` answered done, and Cloud cancelled the run (`:220`), which already held its Build Grant and build secrets. With the grant's Machine unreachable, the budget's Failed "your Machine couldn't be reached to confirm the push" failed the build instead (now `core/crates/ployz/src/sdk/github_build.rs:514-518`). `start_limit_reads_the_grant` puts this back, and the tests' `timed_out()` sets it.

`checkStoreGithubBuild`'s comment allowed the cancel: "GitHub keeps one that started", where it now reads "GitHub keeps one whose check-in committed" (`:187-188`). The Build Order rule says two things (`dashboard/CONTEXT.md:213`). A build moves to the next Builder "only when the current one does not start the build in time", and "A build that has started moves on only when GitHub fails it for infrastructure reasons (the runner stopped before its final report, the run pushed nothing without a failed Build Step, the runner couldn't install ployz, or the run ran out of time)". On GitHub Actions, running out of time is the 2-hour budget: "On GitHub Actions it has 2 hours, and if it runs out of time there, the next builder in your build order takes it" (`docs/user/builds/overview.md:56-57`; `GITHUB_RUN_BUDGET_MS`, `dashboard/src/modules/config-store/store-github-builds.server.ts:33`, counted from the check-in at `dashboard/src/modules/config-store/store-github-builds.server.ts:212`). The start limit wrote that budget's message for a run that checked in after the start limit ran out (`START_WITHIN_MINUTES`, `dashboard/src/modules/config-store/store-github-builds.server.ts:37`; `dashboard/src/modules/config-store/store-deployment.inngest.ts:157-160`), with its budget just begun. And the code answered "in time" two ways. A check-in that commits one step later, after `github_finish`'s re-read and before its settle, was already kept: the Unstarted settle is refused, and `github_finish` answers Waiting, "It checked in meanwhile: the next look finds it started" (`core/crates/ployz/src/sdk/github_build.rs:499-503`). So whether the start limit kept a run that checked in turned on which side of the re-read its check-in committed. Neither sentence of the rule says which side of the start limit a check-in that commits while the start limit settles is on. The property and the code read "in time" as that Waiting does: a run started in time when its check-in committed before the start limit settled. That is this model's reading of the rule, not the rule's words.

The build itself went on: Skipped put it back to pending, and `startLimit` needs a next Builder (`dashboard/src/modules/config-store/store-deployment.inngest.ts:157`), the Servers, which the runner's `run-deployment` step builds on (`core/crates/ployz/src/sdk/store_runner.rs:1576-1600`). What was lost is the GitHub run that already started.

**A Server could report on a build a GitHub run holds.** `record()` checked no holder, so a runner's report on a build a GitHub run held or ended wrote its status, message and Server over the run's (`core/crates/ployz-store/src/build.rs:299-312`). It now refuses one (`core/crates/ployz-store/src/build.rs:289-296`), and the Store reads a `building` row as held by the run or by the Servers, never both (`Stage` and `By`, `core/crates/ployz-store/src/build.rs:391-415`). `record_ignores_github` drops the guard.

**A retried start left the Servers a build its run held.** Found after `record`'s guard was added, in two rounds. When `github-start`'s first try timed out on `githubDispatched`, Cloud cancelled its run (`:166`), and Inngest ran the step again. The re-run dispatched a second run, which `github_dispatched` refused once the first try's write had committed (`core/crates/ployz-store/src/build.rs:1020-1028`). Cloud cancelled that run too and answered `done`, so `walkGithub` returned (`dashboard/src/modules/config-store/store-deployment.inngest.ts:154`) while the first run still held the build, and `run-deployment` sent the building source to the Servers (`targets`, `core/crates/ployz/src/sdk/store_runner.rs:1553-1600`). Before the guard, `record` took their report over the run's hold. With the guard, it refuses their first Building report (`core/crates/ployz/src/sdk/store_runner.rs:940-945`). The runner stops, Cloud reads the conflict as nothing to run (`dashboard/src/modules/config-store/store-deployment.server.ts:107`), and the Deployment lapses Unknown once its runner lease runs out (`core/crates/ployz-store/src/deployment/mod.rs:1293`). A first read that follows a held build (`:118-119`) covers a first try's write that committed before the re-run, but not one that commits after that read. After any refused write, its hand-off, skip or end, the re-run now reads the build again and follows the run that holds it (`refused`, `:122`): those writers refuse only under the Environment lock, so that read sees the winner. A re-run that answers `done` without that read has just committed its own skip or end, which refuses the first try's late hand-off. `start_leaves_held_run` answers `done` after a refused hand-off instead.

**The properties.** `STARTED_RUN_KEPT`, an `always`: "the start limit never ends a run whose check-in committed, nor cancels one while it holds the build". `State::limit_took` is set when the walk's start-limit `github_end` commits on a run whose grant is recorded, any end, or when its `:220` cancel lands while the run still holds the build with its grant. The cancel clause covers only a run that still holds the build. Once another actor ended the hold, that actor's end decided the run, not the start limit: the run's own final report and `github_finish`, after which the run has nothing left to do, or a user's cancel, whose `cancelStoreGithubBuilds` cancels the run itself. The model has one run, so the flag watches every run whose check-in committed.

`NO_SERVER_ON_GITHUB_BUILD`, an `always`: "no Server's report lands on a build a GitHub run holds". `State::server_on_github` is set when `record` takes the Servers' report on a row whose `github` is set.

`SERVERS_REPORT_LANDS`, an `always`: "record never refuses the Servers' report on a build the walk left them". `State::servers_refused` is set when `record` refuses it, which stops the runner.

## Run it

```
cd core && cargo nextest run -p ployz-verify build_handoff --no-capture
```

Expected: the summary line ends with `18 tests run: 18 passed, 201 skipped`. `the_code_keeps_a_started_run` prints `the code: 1407273 states`, `timed_out_breaks_only_the_finding` prints `timed out: 1523471 states`, and `the_check_in_guard_refuses_a_second_grant` prints `two requests: 3234872 states`. Each story prints under its test's name. A warm run took 78 s and 106 s of nextest time in two runs. CI runs `ployz-verify` in its workspace nextest run.

Read the tests in three groups.
- **The finding, with the start limit settling by the grant** (`timed_out()`, which sets `start_limit_reads_the_grant`).
  - `timed_out_passes_on_a_run_that_checked_in` replays the finding's twelve actions with `follow`, through the cancel, and asserts the start limit read no grant before the check-in committed, the Skipped settle, the pending row and the cancel. It then asks the checker for its first broken path (`breaks`) and asserts the same race ends it.
  - `timed_out_fails_a_run_that_checked_in_when_its_machine_is_unreachable` keeps to broken paths whose last step is the Failed "unreachable" settle.
  - `timed_out_breaks_only_the_finding` expects `STARTED_RUN_KEPT` to break, `NO_GRANT_AFTER_CANCEL`, `ONE_GRANT_PER_RUN`, `NO_SERVER_ON_GITHUB_BUILD` and `SERVERS_REPORT_LANDS` to hold, and every `COVERAGE` row to be reached.
  - `timed_out_needs_the_unstarted_guard` also sets `unstarted_ignores_grant`, and keeps to broken paths whose last step is an Unstarted settle: `github_finish`'s re-read found no grant, and a check-in committed after it.
- **The code.**
  - `the_code_keeps_a_started_run` runs `code()` with `assert_properties`: every `always` holds, and every row that is not `Reach::TimedOut` is reached.
  - `the_start_limit_waits_for_the_finding_s_run` replays the finding's schedule on the code: the start limit's Unstarted is refused, `github_finish` answers Waiting, Cloud does not cancel, and the walk opens its next look.
  - `a_later_look_ends_the_run_the_start_limit_waited_for` asks for the first path to each of the rows "after the start limit waited for a run that checked in, an Ended look settles it" and "… the budget settles it".
  - `a_retried_start_follows_the_held_run` asks for the first path, from the retried start, on which the start limit ends the cancelled run Unstarted and the Servers' report lands.
  - `a_retried_start_follows_a_hand_off_that_commits_after_its_read` asks for the first path to the retry's second read following the first try's run: the retry's read finds no run, the first try's write commits, and the retry's hand-off is refused.
  - `a_retried_start_s_run_that_checked_in_keeps_its_build` asks for the first path on which the cancelled run checks in before GitHub ends it: the start limit's Unstarted is refused, and a later look settles the run.
  - `the_start_limit_on_each_edge_state` runs the start limit's `github_finish` on each state its re-read can find: no grant (settles Unstarted), a grant without an end and a grant with its end reported (Waiting, row unchanged), passed on, built and failed (each returned, row unchanged). For another run's build, it runs `github_end`'s Unstarted for `RUN` on a row run 1 holds: it is refused and the row is unchanged. `github_finish` takes the run from its re-read (`core/crates/ployz/src/sdk/github_build.rs:485`), so only `github_end` is ever given a run that may not hold the build.
- **The code's guards.** Each sets one flag on `code_without(..)` (The code's guards): `the_start_limit_needs_its_look`, `the_start_limit_needs_the_unstarted_guard`, `the_start_limit_needs_the_done_gate`, `the_check_in_guard_refuses_a_cancelled_deployment`, `the_check_in_guard_refuses_a_second_grant`, `a_retried_start_needs_its_second_read` and `record_needs_its_github_guard`. The second check-in test runs two check-in requests of one run, first with the guard (every `always` holds), then without it. `a_retried_start_needs_its_second_read` keeps to the path on which the first try's write commits between the retry's read and its hand-off. The walk leaves the Servers no build a run holds, so `record_needs_its_github_guard` first checks that `record_ignores_github` alone breaks nothing, then sets `start_leaves_held_run` too. It also runs `record` with its guard on the start state, where the run holds the build (refused, row unchanged), and on a build GitHub passed on (taken, Building).

## The code's guards

`github_finish` takes the look that calls it (`GithubLook`, `core/crates/ployz/src/sdk/github_build.rs:444-455`), and the start limit issues only Unstarted, whatever the re-read found (`core/crates/ployz/src/sdk/github_build.rs:489-504`).

```text
github_finish("start_limit")   re-read the build; passed on or not building: return it;
                               else settle GithubEnd::Unstarted "no runner started the
                               build in time", whatever the grant. github_end's grant guard
                               refuses it once a check-in committed, and github_finish
                               answers Waiting (core/crates/ployz/src/sdk/github_build.rs:499-503)
github_finish("budget")        as timed_out=true did: an unreachable Machine fails the build,
                               and nothing pushed is Skipped "the run ran out of time"
github_finish("ended")         as timed_out=false did: an unreachable Machine answers Waiting
```

Cloud's walk keeps its control flow, and passes the look (`:209`, `:214`, `:217`, `:219`, `:223`, `:312`): on Waiting the start limit returns waiting (`:221`), so it does not cancel (`:220`), and the walk opens its next look with `GITHUB_CHECK_INTERVAL` (`dashboard/src/modules/config-store/store-deployment.inngest.ts:155-161`). That look ends the run through the Ended look once the run reports or completes (`:223`), or through the budget (`:212-214`).

| Guard | What it keeps | Flag | Test, and the `always` property that breaks without it |
|---|---|---|---|
| The start limit issues only Unstarted (`core/crates/ployz/src/sdk/github_build.rs:489-504`) | A run whose check-in committed inside the window: the start limit's end is refused, not settled | `start_limit_reads_the_grant` | `the_start_limit_needs_its_look`, `STARTED_RUN_KEPT` |
| The Unstarted grant guard (`core/crates/ployz-store/src/build.rs:1248-1250`) | The same run: without it the start limit's Unstarted passes the run on | `unstarted_ignores_grant` | `the_start_limit_needs_the_unstarted_guard`, `STARTED_RUN_KEPT`; with `start_limit_reads_the_grant`, `timed_out_needs_the_unstarted_guard` |
| The done-gate on Cloud's cancel (`:220`) | The same run: on Waiting, Cloud does not cancel it | `cancel_ignores_done` | `the_start_limit_needs_the_done_gate`, `STARTED_RUN_KEPT` |
| The check-in's guard (`core/crates/ployz-store/src/build.rs:1149-1154`) | No check-in for a cancelled Deployment, and no second grant over the first | `check_in_unguarded` | `the_check_in_guard_refuses_a_cancelled_deployment`, `NO_GRANT_AFTER_CANCEL`; `the_check_in_guard_refuses_a_second_grant`, `ONE_GRANT_PER_RUN`, with two requests |
| After a refused write, the retried start reads the build again and follows the run that holds it (`:122`) | The walk returns only once no run holds the build, so the Servers' report lands | `start_leaves_held_run` | `a_retried_start_needs_its_second_read`, `SERVERS_REPORT_LANDS` |
| `record`'s GitHub guard (`core/crates/ployz-store/src/build.rs:289-296`) | No Server's report on a build a GitHub run holds or ended | `record_ignores_github` | `record_needs_its_github_guard`, `NO_SERVER_ON_GITHUB_BUILD`, with `start_leaves_held_run` |

What the code keeps, and what it leaves:
- **A run that checked in and never reports waits for GitHub or the budget.** The next look's `:223` waits until `runEnded` says it completed, or the budget ends it. The rows "after the start limit waited for a run that checked in, an Ended look settles it", "… the budget settles it" and "… the report's own github_finish settles it" reach each end on the code and under `timed_out()`. Under `timed_out()` they are reached by the check-in landing after `github_finish`'s re-read, the path `github_finish`'s Waiting (`core/crates/ployz/src/sdk/github_build.rs:499-503`) was written for.
- **The start limit still cancels a run another actor already ended.** When the run's own report and `github_finish` settled it, or `github_cancel` failed it, inside the window, the start limit's re-read returns it and Cloud cancels the run (`:220`). The run has already sent its final report, or the user's cancel cancels it anyway.
- **One unreachable read still fails a build under the budget** (`core/crates/ployz/src/sdk/github_build.rs:514-518`). That is the budget's choice, not the start limit's.

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
 3. Cloud walk: github-check-0: heldGithubBuild reads it building, with build.grant === null (:194)
    row: building, github run 0, no grant | Deployment: Queued | run: running | walk: Checking { check: 0, ended: false, attempt: 0, at: Deployment { grant: false, ended: false } } | job: Queued
 4. Cloud walk: github-check: the Deployment is queued (:197), and the installation is usable (:202-206); runEnded is next (:217)
    row: building, github run 0, no grant | Deployment: Queued | run: running | walk: Checking { check: 0, ended: false, attempt: 0, at: AskingGithub { grant: false } } | job: Queued
 5. Cloud walk: github-check: runEnded says the run is still running (:177-179); at the start limit it calls finish("start_limit") (:219)
    row: building, github run 0, no grant | Deployment: Queued | run: running | walk: Checking { check: 0, ended: false, attempt: 0, at: Finishing { look: StartLimit, at: Read } } | job: Queued
 6. Runner: check-in: heldGithubBuild and github_authorize read it building with no grant, so the pre-check passes (:280-281, core/crates/ployz/src/sdk/github_build.rs:195-204)
    row: building, github run 0, no grant | Deployment: Queued | run: running | walk: Checking { check: 0, ended: false, attempt: 0, at: Finishing { look: StartLimit, at: Read } } | job: CheckingIn(Minting)
 7. Runner: check-in: the Machine mints a Build Grant (core/crates/ployz/src/sdk/github_build.rs:222)
    row: building, github run 0, no grant | Deployment: Queued | run: running | walk: Checking { check: 0, ended: false, attempt: 0, at: Finishing { look: StartLimit, at: Read } } | job: CheckingIn(Committing)
 8. Runner: check-in: the Store's github_check_in finds the run holding the build, in flight with no grant, and sets the grant and checked_in_at (core/crates/ployz-store/src/build.rs:1141-1173); the runner gets its grant and build secrets (core/crates/ployz/src/sdk/github_build.rs:255-273)
    row: building, github run 0, grant | Deployment: Queued | run: running | walk: Checking { check: 0, ended: false, attempt: 0, at: Finishing { look: StartLimit, at: Read } } | job: Building
 9. Cloud walk: github_finish("start_limit"), settling by the grant as timed_out=true did (under Broken): re-reads the build and finds the grant (core/crates/ployz/src/sdk/github_build.rs:473, :505); inspect_grant is next (core/crates/ployz/src/sdk/github_build.rs:506)
    row: building, github run 0, grant | Deployment: Queued | run: running | walk: Checking { check: 0, ended: false, attempt: 0, at: Finishing { look: StartLimit, at: Inspect { ended: None } } } | job: Building
10. Cloud walk: github_finish("start_limit"), settling by the grant as timed_out=true did (under Broken): inspect_grant reads the grant on its Machine: Pushed(None) (core/crates/ployz/src/sdk/github_build.rs:584-611, :629-630)
    row: building, github run 0, grant | Deployment: Queued | run: running | walk: Checking { check: 0, ended: false, attempt: 0, at: Finishing { look: StartLimit, at: Settle(Skipped(RanOutOfTime)) } } | job: Building
11. Cloud walk: github_finish("start_limit"), settling by the grant as timed_out=true did (under Broken): settle: github_end accepts Skipped "the run ran out of time" from the run that holds the build (core/crates/ployz-store/src/build.rs:1247-1253), and writes it pending (core/crates/ployz-store/src/build.rs:1258-1285)
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

Each flag breaks only the `always` property its test names, and each test fails without its flag. To break a guard that has no flag, edit the model. Each break below was applied alone (B21's two edits together) to `core/crates/ployz-verify/src/build_handoff.rs` at this commit, run with `cd core && cargo nextest run -p ployz-verify build_handoff --no-fail-fast`, and reverted, and turned exactly the tests it names red. B6 to B9, B22 and B24 make a flag do nothing, so the tests that need it go red; `timed_out()` sets `start_limit_reads_the_grant`, so B9 turns its tests red too.

**B1. a timed-out look answers Waiting where it settles Skipped "the run ran out of time".** Red: `the_code_keeps_a_started_run`, `timed_out_breaks_only_the_finding`, `timed_out_passes_on_a_run_that_checked_in`.

```diff
-_ if timed_out => End::Skipped(Why::RanOutOfTime),
+_ if timed_out => return Next::Returned(Finished::Waiting),
```

**B2. a timed-out look reads Unreachable as Waiting.** Red: `the_code_keeps_a_started_run`, `timed_out_breaks_only_the_finding`, `timed_out_fails_a_run_that_checked_in_when_its_machine_is_unreachable`.

```diff
-GrantRead::Unreachable if !timed_out => {
+GrantRead::Unreachable if true => {
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
-broken.unstarted_ignores_grant || github.grant.is_none()
+github.grant.is_none()
```

**B7. check_in_unguarded made a no-op.** Red: `the_check_in_guard_refuses_a_cancelled_deployment`, `the_check_in_guard_refuses_a_second_grant`.

```diff
-if !self.broken.check_in_unguarded && (!wanted || github.grant.is_some()) {
+if !wanted || github.grant.is_some() {
```

**B8. cancel_ignores_done made a no-op.** Red: `the_start_limit_needs_the_done_gate`.

```diff
-(!waiting || self.broken.cancel_ignores_done)
+!waiting
```

**B9. start_limit_reads_the_grant made a no-op.** Red: `the_start_limit_needs_its_look`, `timed_out_breaks_only_the_finding`, `timed_out_fails_a_run_that_checked_in_when_its_machine_is_unreachable`, `timed_out_passes_on_a_run_that_checked_in`.

```diff
-if look == Look::StartLimit && !self.broken.start_limit_reads_the_grant {
+if look == Look::StartLimit {
```

**B10. the start limit's look gone.** Red: `the_check_in_guard_refuses_a_second_grant`, `the_code_keeps_a_started_run`, `the_start_limit_on_each_edge_state`, `the_start_limit_waits_for_the_finding_s_run`.

```diff
-if look == Look::StartLimit && !self.broken.start_limit_reads_the_grant {
+if false && !self.broken.start_limit_reads_the_grant {
```

**B11. the check-in ignores the Deployment.** Red: `the_check_in_guard_refuses_a_second_grant`, `the_code_keeps_a_started_run`, `timed_out_breaks_only_the_finding`.

```diff
-if !self.broken.check_in_unguarded && (!wanted || github.grant.is_some()) {
+if !self.broken.check_in_unguarded && github.grant.is_some() {
```

**B12. the check-in ignores a grant.** Red: `the_check_in_guard_refuses_a_second_grant`.

```diff
-if !self.broken.check_in_unguarded && (!wanted || github.grant.is_some()) {
+if !self.broken.check_in_unguarded && !wanted {
```

**B13. wake does nothing.** Red: `the_code_keeps_a_started_run`, `timed_out_breaks_only_the_finding`.

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

```diff
-if look == Look::StartLimit && !self.broken.start_limit_reads_the_grant {
+if look == Look::StartLimit && !self.broken.start_limit_reads_the_grant && github.grant.is_none() {
```

```diff
-if look == Look::StartLimit && by == Finisher::Walk && !model.broken.start_limit_reads_the_grant {
+if false && look == Look::StartLimit && by == Finisher::Walk && !model.broken.start_limit_reads_the_grant {
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

**B26. the retry's first read never follows a run.** Red: `the_code_keeps_a_started_run`, `timed_out_breaks_only_the_finding`.

```diff
-let follows = state.row.status == Status::Building && state.row.github.is_some();
+let follows = at == Start::Reread && state.row.status == Status::Building && state.row.github.is_some();
```

B15 turns nothing red: the Store's own guard at `core/crates/ployz-store/src/build.rs:1149-1151` refuses the same check-in, so no property needs the pre-check's grant half. It is the code's, so the model keeps it.

To make one `COVERAGE` row unreachable, put `false && ` inside its closure, before the body. A one-line predicate becomes `reached: |_, state| false && took(state, …)`; a block predicate becomes `reached: |_, state| false && { … }`. Each of the 74 rows was made unreachable that way, alone, and turned these tests red:
- the other 67 rows: red `the_code_keeps_a_started_run`, `timed_out_breaks_only_the_finding`.
- "the start limit settles a run that checked in Skipped "the run ran out of time""; "the start limit fails a run that checked in, its grant's Machine unreachable": red `timed_out_breaks_only_the_finding`.
- "after the start limit waited for a run that checked in, an Ended look settles it"; "after the start limit waited for a run that checked in, the budget settles it": red `a_later_look_ends_the_run_the_start_limit_waited_for`, `the_code_keeps_a_started_run`, `timed_out_breaks_only_the_finding`.
- "the first try's hand-off commits after the retry read the build: the retry's hand-off is refused, and its second read follows the first try's run": red `a_retried_start_follows_a_hand_off_that_commits_after_its_read`, `the_code_keeps_a_started_run`, `timed_out_breaks_only_the_finding`.
- "after a retried start, a look passes on the run Cloud cancelled, and the Servers take the build": red `a_retried_start_follows_the_held_run`, `the_code_keeps_a_started_run`, `timed_out_breaks_only_the_finding`.
- "after a retried start, the run Cloud cancelled checked in first: the start limit waits for it, and a later look settles it": red `a_retried_start_s_run_that_checked_in_keeps_its_build`, `the_code_keeps_a_started_run`, `timed_out_breaks_only_the_finding`.

## Bounds and blind spots

The bounds, and why each leaves out no new state:
- **One run, one check-in request by default.** A run's check-in is the one request the model's job sends. `the_check_in_guard_refuses_a_second_grant` runs two (`requests: 2`), for the guard's grant half. The Action's code is not in this repo, so whether it retries a check-in is not shown here. A retried `github-start` can dispatch a second run. The model keeps the run whose hand-off commits as `RUN`: the other never holds the build, Cloud cancelled it, and its check-in and late hand-off are refused, so it changes nothing the model holds (What the model copies).
- **One late write.** Only the first try's hand-off can commit late (`Who::Delayed`), and the model's retry answers. If the retry's own `githubDispatched` times out too, Cloud cancels its run (`:166`) and the next try starts from the same kind of state: a hand-off that may still commit, then `holder`'s read. Once the step's retries run out, the walk never hands the build to the Servers: the function's `onFailure` stops the run (`dashboard/src/modules/config-store/store-deployment.inngest.ts:44-50`), and the Deployment, still unclaimed, fails (`dashboard/src/modules/config-store/store-deployment.server.ts:131-138`, `:186-189`).
- **Two looks** (`MAX_CHECKS`). The finding is at check 0, and check 1 is the later look that ends a run the start limit waited for. With one look (B20 above), the rows "after the start limit waited for a run that checked in, …" are never reached. Lifted to four looks in the scratch lift below, which finds nothing new.
- **One crash by default,** within Inngest's `retries: 2`. Lifted to two crashes in the scratch lift below. `the_check_in_guard_refuses_a_second_grant` runs with no crash.
- **No clock.** The budget (`:212`) can be past whenever the run checked in, and every wait can time out or take an event. That leaves out timing, not orders. A grant's expiry is the clock's too: the model takes it, with a daemon restart, as the grant's Machine losing the grant, at any step. The model says nothing about how long the window is or how often it opens.
- **A next Builder.** The model's walk has one, so check 0 is the start limit (`dashboard/src/modules/config-store/store-deployment.inngest.ts:157`). Without one, `startLimit` is false at every check, and `:218` returns waiting. With one, no look after check 0 reads a run with no grant (the start limit ended it, or its Unstarted was refused for a check-in), so the model has no row for `:218`.
- **An installation the Organization can use** (`:202-206`). Losing it fails the Deployment (`:203-205`) through `cancelStoreGithubBuilds`, which the model runs at `:199`; `stopGithubInstallationBuilds` cancels the run and wakes the walk (`dashboard/src/modules/config-store/config-store.server.ts:80-104`). Neither is the start limit.
- **GitHub ends the run** only after Cloud's cancel or once the job exited. A job inside a request when GitHub ends it finishes that request first (Cloud web serves it) and exits after. The steps a request takes after the run completed read nothing GitHub holds, so the same states are reached as when GitHub ends the run right after the request.
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
            .filter(|row| row.reach == Reach::Both || model.broken.start_limit_reads_the_grant)
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
LIFT timed out, 2 crashes, 4 checks: 7145851 states in 26.947477485s; always broken: ["the start limit never ends a run whose check-in committed, nor cancels one while it holds the build"]; rows unreached: []
LIFT the code, 2 crashes, 4 checks: 6963326 states in 24.355840409s; always broken: []; rows unreached: []
LIFT timed out, 2 requests, 1 crash, 4 checks: 24157764 states in 94.004328264s; always broken: ["the start limit never ends a run whose check-in committed, nor cancels one while it holds the build"]; rows unreached: []
LIFT the code, 2 requests, 1 crash, 4 checks: 23356770 states in 86.840602615s; always broken: []; rows unreached: []
```

The whole command took 328 s wall in one run, release build included.

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
  - **How.** A Machine forgets every grant when its daemon restarts, and a grant once `GRANT_LIFETIME` passes (`core/crates/ployzd/src/management/build_grant.rs:6-9`, `:49`, `:188`, `:207-215`). Its `inspect` then answers NotFound, "this Machine holds no such Build Grant; it expired or the daemon restarted", even for a grant that received the push (`core/crates/ployzd/src/management/build_grant.rs:203-221`). `grant_read` reads NotFound as `Pushed(None)` (`core/crates/ployz/src/sdk/github_build.rs:629-630`). With the run's Built report recorded, `github_finish` settles Skipped "the run ended without pushing an image" at an Ended look (`core/crates/ployz/src/sdk/github_build.rs:549-551`), or "the run ran out of time" at the budget (`core/crates/ployz/src/sdk/github_build.rs:546-548`), in place of Built (`core/crates/ployz/src/sdk/github_build.rs:522-541`). `github_end` puts the build back to pending with that reason (`core/crates/ployz-store/src/build.rs:1277-1285`).
  - **What follows depends on the Build Order.** With the Servers in it, they build it again (`core/crates/ployz/src/sdk/store_runner.rs:1576-1600`). Without them, as in a GitHub-only order, the Deployment fails with, for a Service named `web`, "Build failed. web: the run ended without pushing an image. No other Builder in your Build Order can take it" (`core/crates/ployz/src/sdk/store_runner.rs:1576-1583`, `:1602-1605`; `then_sentence` in `core/crates/ployz/src/ui/error.rs`; `docs/user/troubleshooting/builds.md:110-111`). The model always has a next Builder (A next Builder, under Bounds and blind spots), so its rows cover only the rebuild.
  - **What a user sees.** The build's log gains "GitHub skipped: the run ended without pushing an image" (`core/crates/ployz-store/src/build.rs:1278`). `ployz deploy` prints "GitHub skipped it: the run ended without pushing an image" under the build (`core/crates/ployz/src/handlers/deploy.rs:1575-1576`). Its summary reads "GitHub skipped web: the run ended without pushing an image." until the Servers built it, and "web built on <server>. GitHub skipped it: the run ended without pushing an image." after (`core/crates/ployz/src/handlers/deploy/progress.rs:336`, `:338`). The dashboard's build panel shows the build's message, "the run ended without pushing an image", in red above its log, unless the Deployment's failure reason already carries it (`dashboard/src/routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/-components/deployments/RunPanel.tsx:59-61`), until the Servers' report replaces the message (`core/crates/ployz-store/src/build.rs:299-308`).
  - **Bet 8.** The Machine answered that it holds no such grant, not that nothing was pushed, so "the run ended without pushing an image" is "Certainty the observer lacks" (`core/DESIGN.md:200`).
  - **The model.** The two rows "after the grant's Machine lost a grant that received the push, …" reach the inspect that reads nothing pushed for a Built report, with its Skipped settle next, on the code and under `timed_out()` (`lost_after_built`).
  - **Timing.** The two clocks start at different moments. The grant's 3 hours start at its mint (`core/crates/ployzd/src/management/build_grant.rs:49`, `:183`). The mint finishes before the separate Store call that records the check-in (`core/crates/ployz/src/sdk/github_build.rs:222`, `:247-251`). The Store stamps that call's time as `checked_in_at` (`core/crates/ployz-store/src/build.rs:1159-1161`), and Cloud's 2-hour budget counts from that stamp (`dashboard/src/modules/config-store/store-github-builds.server.ts:33`, `:212`). A daemon restart drops the grant whenever it happens (`core/crates/ployzd/src/management/build_grant.rs:6-9`).
  - **Not known.** Losing the grant does not by itself remove the image: the grant's proxy forwards the push to the Machine's image ingest (`core/crates/ployzd/src/management/build_grant.rs:424-437`, `:470-473`), and the grant itself holds only the pushed digest (`core/crates/ployzd/src/management/build_grant.rs:472-473`). Whether the ingest still serves the image after a real restart or expiry, so that the Servers' build could reuse it, was not measured. Nor was how often a daemon restarts between a push and Cloud's look. No issue is filed.

## Keep the model in step

When a change alters a writer or reader of the build row's GitHub fields, or a step of the walk, change the model in the same PR. Examples: a new `github_finish` caller, a new end, a guard on `record()`, a change to `:212-224`'s order.

1. Update the function that copies the product function you changed, and the `reads_as` arm that prints it:
   - `checkStoreGithubBuild`: `BuildHandoff::check` and `At`.
   - `walkGithub` and `storeGithubBuildOpen`: `BuildHandoff::walk` and `Walk`.
   - `github_finish`, `settle` and `inspect_grant`: `BuildHandoff::finish`, `Finish` and `GrantRead`.
   - `github_end`: `State::github_end`.
   - `github_check_in`, `mint` and the old-mint skip: `BuildHandoff::pre_check`, `check_in` and `CheckIn`.
   - `github_report` and `recordStoreGithubBuildSteps`: the job's `Reporting`, `Finishing` and `Waking` arms.
   - `github_cancel` and `cancelStoreGithubBuilds`: `BuildHandoff::stop` and `Stop`.
   - the user's cancel: `BuildHandoff::user`.
   - `record`: `BuildHandoff::servers` and `Servers`.
   - `startStoreGithubBuild`'s reads and hand-off: `BuildHandoff::start`, `hand_off`, `Start`, `Who::Delayed` and `init_states`'s retried state.
2. If the change adds a guard, add a `Broken` flag that removes it, and a test naming the property it keeps.
3. If the change alters what a look settles, keep `timed_out()` as the start limit before `GithubLook`, and keep its tests, since their printed paths are the record of what the start limit's look keeps out.
4. If the change adds a branch, add a `COVERAGE` row whose predicate requires the branch it names (`took`).
5. Every `State` field is read: `deployment`, `row`, `pushed` (by `finish`'s inspect), `cancelled` and `completed` (by `offers` and `runEnded`), `walk`, `job`, `again`, `user`, `crashes` (by `offers`), `limit_took` (by `STARTED_RUN_KEPT`), `limit_waited` (by the rows), `granted_after_cancel` (by `NO_GRANT_AFTER_CANCEL`), `grant_replaced` (by `ONE_GRANT_PER_RUN`), `retried` (by the rows and `start`), `delayed` (by `offers`), `retry_won` (by the rows), `servers` (by `offers`), `server_on_github` (by `NO_SERVER_ON_GITHUB_BUILD`), `servers_refused` (by `SERVERS_REPORT_LANDS`) and `took` (by the rows). A field that is only written still splits states, so remove one that nothing reads.
6. Move the sync commit below forward only after you confirmed each covered function still reads as the model says.

## Coverage

Each model step is a copy of code at the sync commit. Reread these when a diff touches them:

- `dashboard/src/modules/config-store/store-github-builds.server.ts`: `startStoreGithubBuild`'s follow and its `githubDispatched` call, `checkStoreGithubBuild`, `storeGithubBuildOpen`, `runEnded`, `checkInStoreGithubBuild` and `recordStoreGithubBuildSteps`.
- `dashboard/src/modules/config-store/store-deployment.inngest.ts`: `walkGithub`, the `run-deployment` step's place after the walks, and the function's `retries` and `concurrency`.
- `dashboard/src/modules/config-store/config-store.server.ts`: `cancelDispatchedGithubRun`, `heldGithubBuild`, `cancelStoreGithubBuilds` and `afterWrite`'s cancel.
- `dashboard/src/routes/api/github/-webhook.handler.ts`: the `workflow_run` event.
- `core/crates/ployz/src/sdk/github_build.rs`: `github_check_in`, `mint`, `github_report`, `GithubLook`, `github_finish`, `settle`, `inspect_grant`, `grant_read` and `github_cancel`.
- `core/crates/ployz-store/src/build.rs`: `record`, `Stage`, `GithubHold`, `building_on`, `github_build`, `github_authorize`, `github_check_in`, `github_report`, `github_end` and `github_outstanding`.
- `core/crates/ployz-store/src/deployment/lifecycle.rs`: `cancel`, and the `record` call for a runner's build report.

Sync commit: `56634f719a922e56be1f56e46cabe99a429a8c20`, the commit this model was checked against, and every line cited above is at it. Code in the list above that changed after it has not been rechecked.
