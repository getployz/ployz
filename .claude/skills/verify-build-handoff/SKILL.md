---
name: verify-build-handoff
description: Model-check a Git build handed to a GitHub Actions run, between Cloud's walk that follows the run (github-open, github-wait and github-check steps, checkStoreGithubBuild's start limit, budget and ended looks, github_finish, cancelStoreGithubBuilds) and the runner's check-in, Build Steps reports and push, against a user cancelling the Deployment, GitHub ending the run, an unreachable or refusing grant Machine, a grant Machine that loses the grant, and Inngest running a step again, with the Stateright model in core/crates/ployz-verify/src/build_handoff.rs. Use when a change touches dashboard/src/modules/config-store/store-github-builds.server.ts (checkStoreGithubBuild, storeGithubBuildOpen, checkInStoreGithubBuild, recordStoreGithubBuildSteps), walkGithub in dashboard/src/modules/config-store/store-deployment.inngest.ts, cancelStoreGithubBuilds, cancelDispatchedGithubRun or heldGithubBuild in dashboard/src/modules/config-store/config-store.server.ts, core/crates/ployz/src/sdk/github_build.rs (github_check_in, mint, github_report, github_finish, settle, inspect_grant, grant_read, github_cancel), or core/crates/ployz-store/src/build.rs (building_on, github_dispatched, github_build, github_authorize, github_check_in, github_report, github_end, github_outstanding).
---

# Verify the GitHub build hand-off

The model in `core/crates/ployz-verify/src/build_handoff.rs` explores every interleaving of the processes that read and write one Git build's `config_build` row while a GitHub Actions run holds it. A bare `:n` below is a line of the file named just before it in the same parentheses, and otherwise of `dashboard/src/modules/config-store/store-github-builds.server.ts`. Every path is written in full from the repo root.

## What the model copies

**The actors.** Each is its own process, so one can step between any two of another's Store calls.
- **Cloud's walk.** Cloud's worker runs the Inngest function `run-store-deployment`. Its `walkGithub` (`dashboard/src/modules/config-store/store-deployment.inngest.ts:151-167`) runs, per look, a `github-open` step (`storeGithubBuildOpen`, `:222-224`), a `github-wait` (`step.waitForEvent`), and a `github-check` step that runs `checkStoreGithubBuild` (`:184-219`). Its `store.githubFinish` (`:203`) calls the Store through the native SDK (`core/crates/ployz-sdk/src/config_store.rs:647`) in the worker's own process, so `github_finish`, `settle` and `inspect_grant` (`core/crates/ployz/src/sdk/github_build.rs:454-552`, `core/crates/ployz/src/sdk/github_build.rs:567-594`) run there too. `inspect_grant` dials the grant's Machine from there.
- **The runner.** A GitHub Actions job. Its check-in and its Build Steps reports are HTTP requests Cloud web serves (`:269-284`, `:292-315`). Each calls the Store through the native SDK in Cloud web's process (`core/crates/ployz-sdk/src/config_store.rs:582`, `core/crates/ployz-sdk/src/config_store.rs:619`), which runs `github_check_in` (`core/crates/ployz/src/sdk/github_build.rs:187-274`) or `github_report` (`core/crates/ployz/src/sdk/github_build.rs:384-432`). After a final report, the handler runs its own `githubFinish(false)` (`:303-308`) and sends the run's event (`:309`). The job's own code, the `getployz/build` Action, is not in this repo.
- **A user** cancelling the Deployment. Cloud web commits the Store's `cancel` (`core/crates/ployz-store/src/deployment/lifecycle.rs:1459-1480`), and `afterWrite` then runs `cancelStoreGithubBuilds` (`dashboard/src/modules/config-store/config-store.server.ts:306-310`).
- **GitHub.** It ends the run once its job exits, or after Cloud's cancel (`cancelGithubRun`, `dashboard/src/modules/github/github-build.server.ts:86-95`), and sends the `workflow_run` webhook, which sends the run's event (`dashboard/src/routes/api/github/-webhook.handler.ts:78-83`).
- **The grant's Machine.** It admits one push on a Build Grant: once a tagged manifest is stored, the grant admits no other (`core/crates/ployzd/src/management/build_grant.rs:244`, `:470-476`). It keeps the grant, marked with what it received, in memory until `GRANT_LIFETIME` passes or the daemon stops, and `inspect` reads the push until then (`core/crates/ployzd/src/management/build_grant.rs:6-9`, `:49`, `:188`, `:207-226`). The model lets it lose the grant the row records, once, at any step while the build is on GitHub (`Who::Machine`). After that it admits no push (`core/crates/ployzd/src/management/build_grant.rs:237-245`), and its `inspect` answers NotFound whatever was pushed (`core/crates/ployzd/src/management/build_grant.rs:203-221`), which `grant_read` reads as `Pushed(None)` (`core/crates/ployz/src/sdk/github_build.rs:612-613`).
- **The Store,** Postgres. Each write is one transaction that first takes the Deployment's Environment row lock (`deployment::locked`, `core/crates/ployz-store/src/deployment/mod.rs:1072-1076`; `core/crates/ployz-store/src/scope.rs:568-580`) and then reads the build. Writers run read committed and readers in one snapshot (`core/crates/ployz-store/src/storage/pg.rs:107-120`), so a write sees every write committed before it took the lock, and a read sees one state. Each model write and each model read is one atomic step.

**Every writer of the row's GitHub fields** (`status`, `github`, `github_skipped`, and `message`, the skip reason on a pending row), and what the model does with it:
- `github_dispatched` (`core/crates/ployz-store/src/build.rs:725-754`), from the `github-start` step (`:152-158`). Left out: the model starts once it committed. The `github-start` step runs once, before the walk's first look (`dashboard/src/modules/config-store/store-deployment.inngest.ts:153-154`).
- `github_start`'s reuse and skip ends (`core/crates/ployz/src/sdk/github_build.rs:112-120`) and `githubSkip` (`:112-116`, `core/crates/ployz-sdk/src/config_store.rs:565-568`). Left out: both run inside `github-start`, before dispatch.
- `github_check_in` (`core/crates/ployz-store/src/build.rs:859-888`), from the runner's check-in. Modeled: `BuildHandoff::check_in`'s `Committing` arm.
- `github_end` with `Unstarted`, from the SDK's old-mint skip (`core/crates/ployz/src/sdk/github_build.rs:225-235`). Modeled: the `OldMint` arm.
- `github_report` (`core/crates/ployz-store/src/build.rs:892-935`). Modeled for the final report only (the job's `Reporting` arm). A batch without an end changes only `received` and the log (`core/crates/ployz-store/src/build.rs:918-925`), which nothing that decides the hand-off reads.
- `github_end` from `github_finish`'s `settle` (`core/crates/ployz/src/sdk/github_build.rs:541-552`), from the walk (`:208`, `:211`, `:213`, `:217`) and from the report handler (`:306`). Modeled: `BuildHandoff::finish`, run by the walk and by the job.
- `github_end` Failed "cancelled" from `github_cancel` (`core/crates/ployz/src/sdk/github_build.rs:633-650`), from the walk (`:193`, `:198`) and from a user's cancel (`dashboard/src/modules/config-store/config-store.server.ts:306-310`). Modeled: `BuildHandoff::stop`, run by the walk at `:193` and by the user. The `:198` call needs an installation the Organization can't use, which the model leaves out (Bounds).
- `record()` by the next Builder (`core/crates/ployz-store/src/build.rs:183-225`), from the runner's `run-deployment` step. Left out: it runs only after every walk returned (`dashboard/src/modules/config-store/store-deployment.inngest.ts:62`, `:79`), and the function runs one at a time per Environment (`dashboard/src/modules/config-store/store-deployment.inngest.ts:43`), so it cannot step inside a walk.
- `build::settle`, from the Deployment's `save` once the Deployment is out of flight and shows an end. Left out: it writes `status` and `message` only on a row with `github` NULL, which is `building` only after `record()`, left out above.
- `stopStoreDeploymentRun`'s `cancelStoreGithubBuilds` (`dashboard/src/modules/config-store/store-deployment.server.ts:186`), from the function's `onFailure` and `cancel-store-deployment` (`dashboard/src/modules/config-store/store-deployment.inngest.ts:44-51`, `:135-140`). Left out: it runs once the function failed or was cancelled, after the walk stopped. The model's walk ends `GaveUp` there.

**Every reader that decides from those fields:** `storeGithubBuildOpen` (`:222-224`); `checkStoreGithubBuild`'s `heldGithubBuild` (`:188-189`); `github_finish`'s re-read (`core/crates/ployz/src/sdk/github_build.rs:461-472`); `github_outstanding` (`core/crates/ployz-store/src/build.rs:1005-1019`); the check-in's `heldGithubBuild` (`:274-275`) and `github_authorize` with the SDK's pre-check (`core/crates/ployz/src/sdk/github_build.rs:195-204`); and `planStoreGithubBuilds` (`:83`), which runs before dispatch and is left out.

**What each step sees, and who can change it between steps.**
- The walk's read at `:188` and the Deployment read at `:191` are separate reads, so a check-in, a report, a cancel or a user's cancel can land between them, and after `:191`. The model takes `:191`, `installationRefusal` (`:196`) and the clock's budget compare (`:206`) as one step (`At::Deployment`): `installationRefusal` and the clock read nothing the model holds.
- `runEnded` (`:171-173`) is its own step: GitHub can end the run before it or after it.
- `deployDialOf` (`:202`) reads Cloud's connections, which the model does not hold, so it adds no step. `github_finish`'s re-read, `inspect_grant` and the settle (`core/crates/ployz/src/sdk/github_build.rs:461`, `:491`, `:541-552`) are three steps.
- The finding's window runs from the `:188` read to `github_finish`'s re-read (`core/crates/ployz/src/sdk/github_build.rs:461`). Inside it run the Deployment read (`:191`), `installationRefusal` (`:196`), `runEnded` (`:211`) and `deployDialOf` (`:202`). The model has no clock, so it says nothing about how long the window is.
- The check-in's `heldGithubBuild` (`:274-275`) and its pre-check (`core/crates/ployz/src/sdk/github_build.rs:195-204`) are one step. A build `heldGithubBuild` reads as passed on has `github_skipped` set, and `github_dispatched` refuses such a build (`core/crates/ployz-store/src/build.rs:735-740`), so no write between the two reads makes the second pass where the first failed. Then the mint (`core/crates/ployz/src/sdk/github_build.rs:222`) and the Store's `github_check_in` (`core/crates/ployz/src/sdk/github_build.rs:247-251`) are a step each. `github_input` (`core/crates/ployz/src/sdk/github_build.rs:206-210`) reads only the build's input.
- `github_report`'s `github_authorize` and `github_report` run in one `store_call` closure (`core/crates/ployz/src/sdk/github_build.rs:426-430`) as two transactions. The model takes them as one step: of `github_authorize`'s refusals, only a build with no `github` can follow from a write (`core/crates/ployz-store/src/build.rs:774-777`; the claims are the run's own), and `github_report` refuses that build too (`core/crates/ployz-store/src/build.rs:900-902`).
- `cancelStoreGithubBuilds` is three steps: `github_outstanding`'s read, its `github_end`, and the run's cancel.

**Inngest runs a step at least once.** The function has `retries: 2` (`dashboard/src/modules/config-store/store-deployment.inngest.ts:41`), per step. A `github-check` step can die after an effect landed, or fail when a Store call times out after its work committed (`core/crates/ployz/src/sdk/store_call.rs:45-52`); Inngest then runs it again from `:188` with the same `seen`. The model offers that crash (`Pick::Crash`) inside any `github-check` step, once per run by default. A settle `github_end` refuses that is not Unstarted fails the step too, since `settle` returns the conflict and `storeTry` fails with it (`core/crates/ployz/src/sdk/github_build.rs:541-552`). After two re-runs the function fails (`Walk::GaveUp`).
- A re-run of the start limit's step reopens the finding's window: its `:188` read comes after the crash, when Inngest runs the step again (`retries: 2`, `dashboard/src/modules/config-store/store-deployment.inngest.ts:41`). The model offers the crash at every statement after `:188`, so it explores a re-run's window as it does the first (row "a github-check step dies after an effect landed, and Inngest runs it again").
- A re-run of the start limit's step reaches a new state: once its settle committed, the re-run's `:188` read finds the build passed on and returns done, so the run is never cancelled. The row "a re-run of a github-check step finds the build no longer building" reaches it. That run cannot check in afterwards: its pre-check finds the build passed on.
- A re-run of `github-open` repeats a read, the same as running it later. A wait is not re-run.
- A re-run of `github-start` is out of scope. It would dispatch a second run, which `github_dispatched` refuses (`core/crates/ployz-store/src/build.rs:732-740`), and Cloud cancels it (`:156`). That is the model section's risk 2.

## The finding

The start limit's look reads the build with `build.grant === null` (`:188`, `:210`). The Deployment is queued and GitHub says the run is running, so it calls `finish(true)` (`:213`). Inside that window the runner's check-in commits: its pre-check passes, the Machine mints a grant, and the Store's `github_check_in` sets the grant and `checked_in_at` (`core/crates/ployz-store/src/build.rs:859-888`). `github_finish(timed_out=true)` re-reads the build (`core/crates/ployz/src/sdk/github_build.rs:461`) and finds the grant. `inspect_grant` reads nothing pushed (a fresh grant, `Pushed(None)`), so the end is Skipped "the run ran out of time" (`core/crates/ployz/src/sdk/github_build.rs:529-531`). `github_end` accepts it from the run that holds the build (`core/crates/ployz-store/src/build.rs:953`): the Unstarted grant guard (`core/crates/ployz-store/src/build.rs:950-952`) applies only to Unstarted. The build goes back to pending with `github_skipped` set (`core/crates/ployz-store/src/build.rs:979-987`). `finish` answers done, and Cloud cancels the run (`:214`), which already holds its Build Grant and build secrets.

`checkStoreGithubBuild`'s comment allows this cancel: "A run that hasn't checked in by the start limit (`startLimit`: another Builder follows) is skipped and cancelled; GitHub keeps one that started" (`:181-182`). The Build Order rule says two things (`dashboard/CONTEXT.md:209`). A build moves to the next Builder "only when the current one does not start the build in time", and "A build that has started moves on only when GitHub fails it for infrastructure reasons (the runner stopped before its final report, the run pushed nothing without a failed Build Step, the runner couldn't install ployz, or the run ran out of time)". On GitHub Actions, running out of time is the 2-hour budget: "On GitHub Actions it has 2 hours, and if it runs out of time there, the next builder in your build order takes it" (`docs/user/builds/overview.md:56-57`; `GITHUB_RUN_BUDGET_MS`, `dashboard/src/modules/config-store/store-github-builds.server.ts:33`, counted from the check-in at `dashboard/src/modules/config-store/store-github-builds.server.ts:206`). The finding's start-limit settle writes that budget's message, "the run ran out of time" (`core/crates/ployz/src/sdk/github_build.rs:529-531`), for a run that checked in after the start limit ran out (`START_WITHIN_MINUTES`, `dashboard/src/modules/config-store/store-github-builds.server.ts:37`; `dashboard/src/modules/config-store/store-deployment.inngest.ts:157-160`), with its budget just begun. What is wrong is that the code answers "in time" two ways. A check-in that commits one step later, after `github_finish`'s re-read and before its settle, is kept: the Unstarted settle is refused, and `github_finish` answers Waiting, "It checked in meanwhile: the next look finds it started" (`core/crates/ployz/src/sdk/github_build.rs:484-488`). So whether the start limit keeps a run that checked in turns on which side of the re-read its check-in commits. The model reaches both sides: the finding, and the row "the start limit's Unstarted is refused because the run checked in, and github_finish answers Waiting". Neither sentence of the rule says which side of the start limit a check-in that commits while the start limit settles is on. The property and the fix read "in time" as `github_finish`'s Waiting already does (`core/crates/ployz/src/sdk/github_build.rs:484-488`): a run started in time when its check-in committed before the start limit settled. That is this PR's reading of the rule, not the rule's words.

Which arm applies: with the grant's Machine reachable, `inspect_grant` answers `Pushed`. A fresh grant has received nothing, and a grant the Machine no longer holds reads NotFound, which is `Pushed(None)` too (`core/crates/ployz/src/sdk/github_build.rs:612-613`), so the end is Skipped. With the Machine unreachable (no connection names it, the dial fails, or the read fails without a refusal, `core/crates/ployz/src/sdk/github_build.rs:576-587`, `:622`), `timed_out` makes it Failed "your Machine couldn't be reached to confirm the push" (`core/crates/ployz/src/sdk/github_build.rs:497-501`), and the build fails. A Machine that refuses the read fails it too (`core/crates/ployz/src/sdk/github_build.rs:493-495`). An unreachable Machine is often a one-off, but under `timed_out` the code does not wait for it: one unreachable read fails the build.

The build itself goes on: Skipped puts it back to pending, and `startLimit` needs a next Builder (`dashboard/src/modules/config-store/store-deployment.inngest.ts:157`), the Servers, which the runner's `run-deployment` step builds on (`core/crates/ployz/src/sdk/store_runner.rs:1475-1499`). What is lost is the GitHub run that already started.

**The property** is `STARTED_RUN_KEPT`, an `always`: "the start limit never ends a run whose check-in committed, nor cancels one while it holds the build". `State::limit_took` is set when the walk's start-limit `github_end` commits on a run whose grant is recorded, any end, or when its `:214` cancel lands while the run still holds the build with its grant. The cancel clause covers only a run that still holds the build. Once another actor ended the hold, that actor's end decided the run, not the start limit: the run's own final report and `github_finish`, after which the run has nothing left to do, or a user's cancel, whose `cancelStoreGithubBuilds` cancels the run itself. The model has one run, so the flag watches every run whose check-in committed.

## Run it

```
cd core && cargo nextest run -p ployz-verify build_handoff --no-capture
```

Expected: the summary line ends with `13 tests run: 13 passed, 120 skipped`. `current_code_breaks_only_the_finding` prints `as is: 324534 states`, `the_fix_keeps_a_started_run` prints `fixed: 302372 states`, and `the_check_in_guard_refuses_a_second_grant` prints `two requests: 647879 states`. Each story prints under its test's name. A warm run takes about 9 s of nextest time. CI runs `ployz-verify` in its workspace nextest run.

Read the tests in four groups.
- **The finding, on the code as it is.**
  - `current_code_passes_on_a_run_that_checked_in` replays the finding's twelve actions with `follow`, through the cancel, and asserts the start limit read no grant before the check-in committed, the Skipped settle, the pending row and the cancel. It then asks the checker for its first broken path (`breaks`) and asserts the same race ends it.
  - `current_code_fails_a_run_that_checked_in_when_its_machine_is_unreachable` keeps to broken paths whose last step is the Failed "unreachable" settle.
  - `current_code_breaks_only_the_finding` expects `STARTED_RUN_KEPT` to break, `NO_GRANT_AFTER_CANCEL` and `ONE_GRANT_PER_RUN` to hold, and every `COVERAGE` row to be reached.
  - `current_code_needs_the_unstarted_guard` sets `unstarted_ignores_grant` on the code as it is, and keeps to broken paths whose last step is an Unstarted settle: `github_finish`'s re-read found no grant, and a check-in committed after it.
- **The code's guards.** `the_check_in_guard_refuses_a_cancelled_deployment` and `the_check_in_guard_refuses_a_second_grant` set `check_in_unguarded`. The second runs two check-in requests of one run, first with the guard (every `always` holds), then without it.
- **The fix.**
  - `the_fix_keeps_a_started_run` runs `fixed()` with `assert_properties`: every `always` holds, and every row that is not `Reach::AsIs` is reached.
  - `the_fix_waits_for_the_finding_s_run` replays the finding's schedule under the fix: the start limit's Unstarted is refused, `github_finish` answers Waiting, Cloud does not cancel, and the walk opens its next look.
  - `the_fix_ends_the_started_run_through_a_later_look` asks for the first path to each of the rows "after the start limit waited for a run that checked in, an Ended look settles it" and "… the budget settles it".
  - `the_fix_s_start_limit_on_each_edge_state` runs the fix's start-limit `github_finish` on each state its re-read can find: no grant (settles Unstarted), a grant without an end and a grant with its end reported (Waiting, row unchanged), passed on, built and failed (each returned, row unchanged). For another run's build, it runs `github_end`'s Unstarted for `RUN` on a row run 1 holds: it is refused and the row is unchanged. `github_finish` takes the run from its re-read (`core/crates/ployz/src/sdk/github_build.rs:473`), so only `github_end` is ever given a run that may not hold the build.
- **The fix's parts.** `the_fix_needs_the_unstarted_guard`, `the_fix_needs_the_done_gate` and `the_fix_needs_its_start_limit_look` each set one flag on `fixed_without(..)` (The fix).

## The fix

The fix is a design for a later PR: `github_finish` takes the look that calls it, and `Look::StartLimit` issues only Unstarted. No product code has it yet.

```text
github_finish(Look::StartLimit)   re-read the build; passed on or not building: return it,
                                  as today; else settle GithubEnd::Unstarted
                                  "no runner started the build in time", whatever the grant.
                                  github_end's grant guard refuses it once a check-in
                                  committed, and github_finish answers Waiting
                                  (core/crates/ployz/src/sdk/github_build.rs:484-488)
Look::Budget, Look::Ended         as timed_out true and false do today
```

Cloud's walk keeps its control flow, and only its call passes the look in place of `timed_out` (`:203`, `:306`): on Waiting the start limit returns waiting (`:215`), so it does not cancel (`:214`), and the walk opens its next look with `GITHUB_CHECK_INTERVAL` (`dashboard/src/modules/config-store/store-deployment.inngest.ts:155-161`). That look ends the run through the Ended look once the run reports or completes (`:217`), or through the budget (`:206-208`).

| Part | What it keeps | Flag | Test, and the `always` property that breaks without it |
|---|---|---|---|
| The start limit issues only Unstarted | A run whose check-in committed inside the window: the start limit's end is refused, not settled | `fix_start_limit_reads_the_grant` | `the_fix_needs_its_start_limit_look`, `STARTED_RUN_KEPT` |
| The Unstarted grant guard (`core/crates/ployz-store/src/build.rs:950-952`), the code's | The same run: without it the fix's Unstarted passes the run on | `unstarted_ignores_grant` | `the_fix_needs_the_unstarted_guard`, `STARTED_RUN_KEPT`; and on the code as it is, `current_code_needs_the_unstarted_guard` |
| The done-gate on Cloud's cancel (`:214`), the code's | The same run: on Waiting, Cloud does not cancel it | `cancel_ignores_done` | `the_fix_needs_the_done_gate`, `STARTED_RUN_KEPT` |
| The check-in's guard (`core/crates/ployz-store/src/build.rs:870-872`), the code's | No check-in for a cancelled Deployment, and no second grant over the first | `check_in_unguarded` | `the_check_in_guard_refuses_a_cancelled_deployment`, `NO_GRANT_AFTER_CANCEL`; `the_check_in_guard_refuses_a_second_grant`, `ONE_GRANT_PER_RUN`, with two requests |

What the fix keeps, and what the later PR must decide:
- **A run that checked in and never reports waits for GitHub or the budget.** The next look's `:217` waits until `runEnded` says it completed, or the budget ends it. The rows "after the start limit waited for a run that checked in, an Ended look settles it", "… the budget settles it" and "… the report's own github_finish settles it" reach each end on both models. On the code as it is they are reached by the check-in landing after `github_finish`'s re-read, the path `github_finish`'s Waiting (`core/crates/ployz/src/sdk/github_build.rs:484-488`) was written for.
- **The start limit still cancels a run another actor already ended.** When the run's own report and `github_finish` settled it, or `github_cancel` failed it, inside the window, the start limit's re-read returns it and Cloud cancels the run (`:214`). The run has already sent its final report, or the user's cancel cancels it anyway.
- **One unreachable read still fails a build under the budget** (`core/crates/ployz/src/sdk/github_build.rs:497-501`). That is the budget's choice, not the start limit's.

## Read a counterexample

A story prints when a test finds its path, under `--no-capture`: `breaks` and `breaks_where` print the path they find, under the test name, the property and the model's flags, and `follow` prints the schedule it replays, with "a schedule" in place of the property. A story is numbered and cut at the first state in BFS order that breaks the property (for a `sometimes`, the first that reaches it), so its last row is that state. `breaks` keeps that first path with a single-thread visitor and stops at the first discovery. `breaks_where` keeps the first path that also passes a filter on the path, and explores the whole model. One thread keeps the path the same from run to run.

Some tests print a story before they fail, and some print none. To print the story for a failing property, keep the edit that turned the test red, add a scratch test to the `tests` module naming the model and property that broke, and run it alone:

```rust
#[test]
fn scratch() {
    breaks(fixed(), STARTED_RUN_KEPT);
}
```

```
cd core && cargo nextest run -p ployz-verify build_handoff::tests::scratch --no-capture
```

`reads_as` writes each row's words. A model edit that changes what a step does needs its `reads_as` arm changed with it, or the story describes the old step. A row nothing reaches has no story: the checker found no state for it.

These are rows 3 to 11 of `current_code_passes_on_a_run_that_checked_in`'s second story, the checker's first broken path:

```
 3. Cloud walk: github-check-0: heldGithubBuild reads it building, with build.grant === null (:188)
    row: building, github run 0, no grant | Deployment: Queued | run: running | walk: Checking { check: 0, ended: false, attempt: 0, at: Deployment { grant: false, ended: false } } | job: Queued
 4. Cloud walk: github-check: the Deployment is queued (:191), and the installation is usable (:196-200); runEnded is next (:211)
    row: building, github run 0, no grant | Deployment: Queued | run: running | walk: Checking { check: 0, ended: false, attempt: 0, at: AskingGithub { grant: false } } | job: Queued
 5. Cloud walk: github-check: runEnded says the run is still running (:171-173); at the start limit it calls finish(true) (:213)
    row: building, github run 0, no grant | Deployment: Queued | run: running | walk: Checking { check: 0, ended: false, attempt: 0, at: Finishing { look: StartLimit, at: Read } } | job: Queued
 6. Runner: check-in: heldGithubBuild and github_authorize read it building with no grant, so the pre-check passes (:274-275, core/crates/ployz/src/sdk/github_build.rs:195-204)
    row: building, github run 0, no grant | Deployment: Queued | run: running | walk: Checking { check: 0, ended: false, attempt: 0, at: Finishing { look: StartLimit, at: Read } } | job: CheckingIn(Minting)
 7. Runner: check-in: the Machine mints a Build Grant (core/crates/ployz/src/sdk/github_build.rs:222)
    row: building, github run 0, no grant | Deployment: Queued | run: running | walk: Checking { check: 0, ended: false, attempt: 0, at: Finishing { look: StartLimit, at: Read } } | job: CheckingIn(Committing)
 8. Runner: check-in: the Store's github_check_in finds the run holding the build, in flight with no grant, and sets the grant and checked_in_at (core/crates/ployz-store/src/build.rs:859-888); the runner gets its grant and build secrets (core/crates/ployz/src/sdk/github_build.rs:255-273)
    row: building, github run 0, grant | Deployment: Queued | run: running | walk: Checking { check: 0, ended: false, attempt: 0, at: Finishing { look: StartLimit, at: Read } } | job: Building
 9. Cloud walk: github_finish(timed_out=true): re-reads the build and finds the grant (core/crates/ployz/src/sdk/github_build.rs:461, :490); inspect_grant is next (core/crates/ployz/src/sdk/github_build.rs:491)
    row: building, github run 0, grant | Deployment: Queued | run: running | walk: Checking { check: 0, ended: false, attempt: 0, at: Finishing { look: StartLimit, at: Inspect { ended: None } } } | job: Building
10. Cloud walk: github_finish(timed_out=true): inspect_grant reads the grant on its Machine: Pushed(None) (core/crates/ployz/src/sdk/github_build.rs:567-594, :612-613)
    row: building, github run 0, grant | Deployment: Queued | run: running | walk: Checking { check: 0, ended: false, attempt: 0, at: Finishing { look: StartLimit, at: Settle(Skipped(RanOutOfTime)) } } | job: Building
11. Cloud walk: github_finish(timed_out=true): settle: github_end accepts Skipped "the run ran out of time" from the run that holds the build (core/crates/ployz-store/src/build.rs:949-955), and writes it pending (core/crates/ployz-store/src/build.rs:960-987)
    row: pending, github none, github_skipped | Deployment: Queued | run: running | walk: Checking { check: 0, ended: false, attempt: 0, at: LimitCancel { waiting: false } } | job: Building
```

Each row is one step, named for who takes it: Cloud walk, Runner, Runner's second check-in (with two requests), User, GitHub or Grant's Machine. A bare `:n` in a row follows the same rule as above. The line under it is the state after the step:
- `row` is the build's `config_build` row: its status, its `github` (run, grant, the final report's end) and whether `github_skipped` is set. "grant" is the job's request's grant, "second grant" the second request's.
- `Deployment` is queued or cancelled. `run` is whether GitHub says the run completed, whether Cloud sent its cancel, whether the runner pushed, and whether the grant's Machine lost the grant ("grant lost").
- `walk` and `job` are each actor's next step: `Walk::Checking`'s `at` names the `checkStoreGithubBuild` statement next, and `attempt` counts the step's runs before this one.

A row that skips a guard says "(under Broken)" when a flag is set.

A test that fails inside `assert_properties` (`the_fix_keeps_a_started_run`) reports only the first property it finds broken, and prints the raw actions instead of the rows. An action is `Action { who, pick }`: `who` is `Walk`, `Job`, `Again`, `User`, `Github` or `Machine`, and `pick` is `Go` or the choice the step made (`PastBudget`, `GithubDown`, `Grant(..)`, `Mint(..)`, `Push`, `ReportBuilt`, `ReportFailed`, `Quit`, `Crash`). To see the rows, call `breaks` with that model and property in a scratch test.

## Real bug or model error

A counterexample is a **lead**, not a verdict. Before calling it a bug, confirm each step of the story against the line it cites. Each `At`, `Finish`, `Job`, `Branch` and `Why` variant's doc comment names its lines, and `reads_as` prints the statement each step copies.

- If a step does something the code cannot, fix the model and say so. The usual suspects:
  - `offers`, which decides what each actor can do in each state;
  - `BuildHandoff::check`, for `checkStoreGithubBuild`'s ladder;
  - `BuildHandoff::finish`, for `github_finish` and `settle`;
  - `State::github_end`, `check_in`, `pre_check` and the job's `Reporting` arm, for the Store's guards.
- If every step matches the code, it is a finding. Keep the property and the model as they are, since weakening either to go green hides the bug.

## Breaking a guard yourself

Set its `Broken` flag through `as_is_without` or `fixed_without`. That changes one model, so only the tests that set the flag see it. Add `--no-fail-fast` to the nextest command, so every red test is listed.

Each flag breaks only the `always` property its test names, and each test fails without its flag. To break a guard that has no flag, edit the model. Each break below was applied alone (B21's two edits together) to `core/crates/ployz-verify/src/build_handoff.rs` at this commit, run with `cd core && cargo nextest run -p ployz-verify build_handoff --no-fail-fast`, and reverted, and turned exactly the tests it names red. B6 to B9 make a flag do nothing, so the tests that need it go red.

**B1. the timed-out look answers Waiting where it settles Skipped "the run ran out of time".** Red: `current_code_breaks_only_the_finding`, `current_code_passes_on_a_run_that_checked_in`, `the_fix_keeps_a_started_run`.

```diff
-                        _ if timed_out => End::Skipped(Why::RanOutOfTime),
+                        _ if timed_out => return Next::Returned(Finished::Waiting),
```

**B2. the code's timed-out look reads Unreachable as Waiting.** Red: `current_code_breaks_only_the_finding`, `current_code_fails_a_run_that_checked_in_when_its_machine_is_unreachable`, `the_fix_keeps_a_started_run`.

```diff
-                    GrantRead::Unreachable if !timed_out => {
+                    GrantRead::Unreachable if true => {
```

**B3. the property's settle clause dropped.** Red: `current_code_breaks_only_the_finding`, `current_code_fails_a_run_that_checked_in_when_its_machine_is_unreachable`, `current_code_needs_the_unstarted_guard`, `current_code_passes_on_a_run_that_checked_in`, `the_fix_needs_its_start_limit_look`, `the_fix_needs_the_unstarted_guard`.

```diff
                         state.took = step(FinishStep::Settled(end));
-                        if limit && started {
-                            state.limit_took = true;
-                        }
```

**B4. the property's cancel clause dropped.** Red: `the_fix_needs_the_done_gate`.

```diff
-                if state.row.started_on(RUN) {
-                    state.limit_took = true;
-                }
-                state.cancelled = true;
-                state.took = Some(Branch::Cancelled(Canceller::StartLimit));
+                state.cancelled = true;
+                state.took = Some(Branch::Cancelled(Canceller::StartLimit));
```

**B5. the settle clause's `started` is always false.** Red: `current_code_breaks_only_the_finding`, `current_code_fails_a_run_that_checked_in_when_its_machine_is_unreachable`, `current_code_needs_the_unstarted_guard`, `current_code_passes_on_a_run_that_checked_in`, `the_fix_ends_the_started_run_through_a_later_look`, `the_fix_keeps_a_started_run`, `the_fix_needs_its_start_limit_look`, `the_fix_needs_the_unstarted_guard`, `the_fix_waits_for_the_finding_s_run`.

```diff
-                let started = state.row.started_on(RUN);
-                let limit
+                let started = false;
+                let limit
```

**B6. unstarted_ignores_grant made a no-op.** Red: `current_code_needs_the_unstarted_guard`, `the_fix_needs_the_unstarted_guard`.

```diff
-broken.unstarted_ignores_grant || github.grant.is_none()
+github.grant.is_none()
```

**B7. check_in_unguarded made a no-op.** Red: `the_check_in_guard_refuses_a_cancelled_deployment`, `the_check_in_guard_refuses_a_second_grant`.

```diff
-if !self.broken.check_in_unguarded && (!wanted || github.grant.is_some()) {
+if !wanted || github.grant.is_some() {
```

**B8. cancel_ignores_done made a no-op.** Red: `the_fix_needs_the_done_gate`.

```diff
-(!waiting || self.broken.cancel_ignores_done)
+!waiting
```

**B9. fix_start_limit_reads_the_grant made a no-op.** Red: `the_fix_needs_its_start_limit_look`.

```diff
-                if self.fix
-                    && look == Look::StartLimit
-                    && !self.broken.fix_start_limit_reads_the_grant
+                if self.fix
+                    && look == Look::StartLimit
```

**B10. the fix's start limit gone.** Red: `the_check_in_guard_refuses_a_second_grant`, `the_fix_keeps_a_started_run`, `the_fix_s_start_limit_on_each_edge_state`, `the_fix_waits_for_the_finding_s_run`.

```diff
-                if self.fix
-                    && look == Look::StartLimit
-                    && !self.broken.fix_start_limit_reads_the_grant
+                if false
+                    && !self.broken.fix_start_limit_reads_the_grant
```

**B11. the code's check-in ignores the Deployment.** Red: `current_code_breaks_only_the_finding`, `the_check_in_guard_refuses_a_second_grant`, `the_fix_keeps_a_started_run`.

```diff
-if !self.broken.check_in_unguarded && (!wanted || github.grant.is_some()) {
+if !self.broken.check_in_unguarded && github.grant.is_some() {
```

**B12. the code's check-in ignores a grant.** Red: `the_check_in_guard_refuses_a_second_grant`.

```diff
-if !self.broken.check_in_unguarded && (!wanted || github.grant.is_some()) {
+if !self.broken.check_in_unguarded && !wanted {
```

**B13. wake does nothing.** Red: `current_code_breaks_only_the_finding`, `the_fix_keeps_a_started_run`.

```diff
-        if let Walk::Waiting {
-            check,
-            woken: false,
-        } = self.walk
-        {
+        if let Walk::Waiting {
+            check,
+            woken: false,
+        } = self.walk
+            && false
+        {
```

**B14. Inngest never retries.** Red: `current_code_breaks_only_the_finding`, `the_fix_keeps_a_started_run`.

```diff
-        let retried = attempt < RETRIES;
+        let retried = false;
```

**B15. the pre-check ignores the grant.** Red: none (see below).

```diff
-            .is_some_and(|github| github.grant.is_none());
-        state.took = Some(Branch::PreCheck { request, pass });
+            .is_some();
+        state.took = Some(Branch::PreCheck { request, pass });
```

**B16. runEnded always running.** Red: `current_code_breaks_only_the_finding`, `the_fix_keeps_a_started_run`.

```diff
-let says_ended = state.completed && !down;
+let says_ended = false && !down;
```

**B17. the start limit also cancels when finish answers Waiting.** Red: `current_code_passes_on_a_run_that_checked_in`, `the_check_in_guard_refuses_a_second_grant`, `the_fix_keeps_a_started_run`, `the_fix_waits_for_the_finding_s_run`.

```diff
-(!waiting || self.broken.cancel_ignores_done)
+(true || self.broken.cancel_ignores_done)
```

**B18. the grant's Machine never loses a grant.** Red: `current_code_breaks_only_the_finding`, `the_fix_keeps_a_started_run`.

```diff
-                if granted && !state.lost {
+                if false && granted && !state.lost {
```

**B19. inspect reads a lost grant's push.** Red: `current_code_breaks_only_the_finding`, `the_fix_keeps_a_started_run`.

```diff
-                let read_push = state.pushed && !state.lost;
+                let read_push = state.pushed;
```

**B20. one look.** Red: `current_code_breaks_only_the_finding`, `the_fix_ends_the_started_run_through_a_later_look`, `the_fix_keeps_a_started_run`.

```diff
-const MAX_CHECKS: u8 = 2;
+const MAX_CHECKS: u8 = 1;
```

**B21. the fix's start limit settles Skipped once the run checked in.** Two edits, applied together: the first makes the fix's start limit settle as the code does whenever the re-read finds a grant, and the second keeps that step's `reads_as` in step. Red: `the_check_in_guard_refuses_a_second_grant`, `the_fix_keeps_a_started_run`, `the_fix_s_start_limit_on_each_edge_state`, `the_fix_waits_for_the_finding_s_run`, the same set as B10. With both edits, the scratch story `breaks(fixed(), STARTED_RUN_KEPT)` reads at row 9 "re-reads the build and finds the grant …; inspect_grant is next", and settles Skipped "the run ran out of time" at row 11.

```diff
                     && !self.broken.fix_start_limit_reads_the_grant
+                    && github.grant.is_none()
                 {
```

```diff
-                    if model.fix && look == Look::StartLimit && by == Finisher::Walk && !model.broken.fix_start_limit_reads_the_grant {
+                    if false && model.fix && look == Look::StartLimit && by == Finisher::Walk && !model.broken.fix_start_limit_reads_the_grant {
```

B15 turns nothing red: the Store's own guard at `core/crates/ployz-store/src/build.rs:870` refuses the same check-in, so no property needs the pre-check's grant half. It is the code's, so the model keeps it.

To make one `COVERAGE` row unreachable, put `false && ` inside its closure, before the body. A one-line predicate becomes `reached: |_, state| false && took(state, …)`; a block predicate, as 33 rows have, becomes `reached: |_, state| false && { … }`. Each row was made unreachable that way, alone, and turned these tests red:
- the other 62 rows, the two grant-loss rows among them: `current_code_breaks_only_the_finding`, `the_fix_keeps_a_started_run`.
- "the start limit settles a run that checked in Skipped "the run ran out of time""; "the start limit fails a run that checked in, its grant's Machine unreachable": red `current_code_breaks_only_the_finding`.
- "after the start limit waited for a run that checked in, an Ended look settles it"; "after the start limit waited for a run that checked in, the budget settles it": red `current_code_breaks_only_the_finding`, `the_fix_ends_the_started_run_through_a_later_look`, `the_fix_keeps_a_started_run`.

## Bounds and blind spots

The bounds, and why each leaves out no new state:
- **One run, one check-in request by default.** A run's check-in is the one request the model's job sends. `the_check_in_guard_refuses_a_second_grant` runs two (`requests: 2`), for the guard's grant half. The Action's code is not in this repo, so whether it retries a check-in is not shown here. A second run is risk 2, out of scope (What the model copies).
- **Two looks** (`MAX_CHECKS`). The finding is at check 0, and check 1 is the later look that ends a run the start limit waited for. With one look (B20 above), the rows "after the start limit waited for a run that checked in, …" are never reached. Lifted to four looks in the scratch lift below, which finds nothing new.
- **One crash by default,** within Inngest's `retries: 2`. Lifted to two crashes in the scratch lift below. `the_check_in_guard_refuses_a_second_grant` runs with no crash.
- **No clock.** The budget (`:206`) can be past whenever the run checked in, and every wait can time out or take an event. That leaves out timing, not orders. A grant's expiry is the clock's too: the model takes it, with a daemon restart, as the grant's Machine losing the grant, at any step. The model says nothing about how long the window is or how often it opens.
- **A next Builder.** The model's walk has one, so check 0 is the start limit (`dashboard/src/modules/config-store/store-deployment.inngest.ts:157`). Without one, `startLimit` is false at every check, and `:212` returns waiting. With one, no look after check 0 reads a run with no grant (the start limit ended it, or its Unstarted was refused for a check-in), so the model has no row for `:212`.
- **An installation the Organization can use** (`:196-200`). Losing it fails the Deployment (`:197-199`) through `cancelStoreGithubBuilds`, which the model runs at `:193`; `stopGithubInstallationBuilds` cancels the run and wakes the walk (`dashboard/src/modules/config-store/config-store.server.ts:80-104`). Neither is the start limit.
- **GitHub ends the run** only after Cloud's cancel or once the job exited. A job inside a request when GitHub ends it finishes that request first (Cloud web serves it) and exits after. The steps a request takes after the run completed read nothing GitHub holds, so the same states are reached as when GitHub ends the run right after the request.
- **Events.** A wait gets the run's event only while it waits; an event sent at another time is lost, as `step.waitForEvent` loses it.

The lift. Set `const MAX_CHECKS: u8 = 4;`, add this test to the `tests` module, run it in release, and revert both:

```rust
#[test]
fn scratch_lifts() {
    for (name, model) in [
        ("as is, 2 crashes", BuildHandoff { crashes: 2, ..as_is() }),
        ("fixed, 2 crashes", BuildHandoff { crashes: 2, ..fixed() }),
        ("as is, 2 requests, 1 crash", BuildHandoff { requests: 2, ..as_is() }),
        ("fixed, 2 requests, 1 crash", BuildHandoff { requests: 2, ..fixed() }),
    ] {
        let start = std::time::Instant::now();
        let checker = model.checker().threads(4).spawn_bfs().join();
        let broken: Vec<_> = [STARTED_RUN_KEPT, NO_GRANT_AFTER_CANCEL, ONE_GRANT_PER_RUN]
            .into_iter()
            .filter(|property| checker.discovery(property).is_some())
            .collect();
        let unreached: Vec<_> = COVERAGE
            .iter()
            .filter(|row| row.reach == Reach::Both || !model.fix)
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
LIFT as is, 2 crashes, 4 checks: 1620124 states in 5.217846995s; always broken: ["the start limit never ends a run whose check-in committed, nor cancels one while it holds the build"]; rows unreached: []
LIFT fixed, 2 crashes, 4 checks: 1585208 states in 4.816669844s; always broken: []; rows unreached: []
LIFT as is, 2 requests, 1 crash, 4 checks: 5461476 states in 16.953846353s; always broken: ["the start limit never ends a run whose check-in committed, nor cancels one while it holds the build"]; rows unreached: []
LIFT fixed, 2 requests, 1 crash, 4 checks: 5302453 states in 17.03016318s; always broken: []; rows unreached: []
```

The whole command took 53 s wall in one run whose release dependencies were already built, and 126 s in another from a cold release build.

Each as-is model breaks only `STARTED_RUN_KEPT`, each fixed model breaks no `always` property, and every row its model can reach is reached.

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

Each line starts from the repo root and changes into `core` or `dashboard` itself. Expected: the first's summary line ends with `9 tests run: 9 passed, 22 skipped`. The second installs the dashboard's packages, then `pnpm sdk` runs `core/scripts/build-cloud-sdk.sh` (`dashboard/package.json:27`). That script fetches the native SDK the third loads from the artifact cache worktrees share, keyed by the SDK's sources, and builds it with Cargo only when the cache has no match (`core/scripts/build-cloud-sdk.sh:9`, `:20-23`, `:31-45`); it then runs the SDK's contract and Node tests (`core/scripts/build-cloud-sdk.sh:70-71`). Without it, the third fails with `Cannot find module './ployz-sdk.node'`. The third starts its own Postgres through Docker and prints `Tests  4 passed (4)`.

## Follow-ups

- **A run that pushed and reported Built can be passed on as one that "ended without pushing an image".** This is on main, apart from the start limit.
  - **How.** A Machine forgets every grant when its daemon restarts, and a grant once `GRANT_LIFETIME` passes (`core/crates/ployzd/src/management/build_grant.rs:6-9`, `:49`, `:188`, `:207-215`). Its `inspect` then answers NotFound, "this Machine holds no such Build Grant; it expired or the daemon restarted", even for a grant that received the push (`core/crates/ployzd/src/management/build_grant.rs:203-221`). `grant_read` reads NotFound as `Pushed(None)` (`core/crates/ployz/src/sdk/github_build.rs:612-613`). With the run's Built report recorded, `github_finish` settles Skipped "the run ended without pushing an image" at an Ended look (`core/crates/ployz/src/sdk/github_build.rs:532-534`), or "the run ran out of time" at the budget (`core/crates/ployz/src/sdk/github_build.rs:529-531`), in place of Built (`core/crates/ployz/src/sdk/github_build.rs:505-524`). `github_end` puts the build back to pending with that reason (`core/crates/ployz-store/src/build.rs:979-987`).
  - **What follows depends on the Build Order.** With the Servers in it, they build it again (`core/crates/ployz/src/sdk/store_runner.rs:1475-1499`). Without them, as in a GitHub-only order, the Deployment fails with, for a Service named `web`, "Build failed. web: the run ended without pushing an image. No other Builder in your Build Order can take it" (`core/crates/ployz/src/sdk/store_runner.rs:1475-1482`, `:1501-1504`; `then_sentence`, `core/crates/ployz/src/ui/error.rs:223-236`; `docs/user/troubleshooting/builds.md:106-107`). The model always has a next Builder (A next Builder, under Bounds and blind spots), so its rows cover only the rebuild.
  - **What a user sees.** The build's log gains "GitHub skipped: the run ended without pushing an image" (`core/crates/ployz-store/src/build.rs:980`). `ployz deploy` prints "GitHub skipped it: the run ended without pushing an image" under the build (`core/crates/ployz/src/handlers/deploy.rs:1567-1568`). Its summary reads "GitHub skipped web: the run ended without pushing an image." until the Servers built it, and "web built on <server>. GitHub skipped it: the run ended without pushing an image." after (`core/crates/ployz/src/handlers/deploy/progress.rs:330`, `:332`). The dashboard's build panel shows the build's message, "the run ended without pushing an image", in red above its log, unless the Deployment's failure reason already carries it (`dashboard/src/routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/-components/deployments/RunPanel.tsx:59-60`), until the Servers' report replaces the message (`core/crates/ployz-store/src/build.rs:210-219`).
  - **Bet 8.** The Machine answered that it holds no such grant, not that nothing was pushed, so "the run ended without pushing an image" is "Certainty the observer lacks" (`core/DESIGN.md:200`).
  - **The model.** The two rows "after the grant's Machine lost a grant that received the push, …" reach the inspect that reads nothing pushed for a Built report, with its Skipped settle next, on both models (`lost_after_built`).
  - **Timing.** The two clocks start at different moments. The grant's 3 hours start at its mint (`core/crates/ployzd/src/management/build_grant.rs:49`, `:183`). The mint finishes before the separate Store call that records the check-in (`core/crates/ployz/src/sdk/github_build.rs:222`, `:247-251`). The Store stamps that call's time as `checked_in_at` (`core/crates/ployz-store/src/build.rs:874-879`), and Cloud's 2-hour budget counts from that stamp (`dashboard/src/modules/config-store/store-github-builds.server.ts:33`, `:206`). A daemon restart drops the grant whenever it happens (`core/crates/ployzd/src/management/build_grant.rs:6-9`).
  - **Not known.** Losing the grant does not by itself remove the image: the grant's proxy forwards the push to the Machine's image ingest (`core/crates/ployzd/src/management/build_grant.rs:424-437`, `:470-473`), and the grant itself holds only the pushed digest (`core/crates/ployzd/src/management/build_grant.rs:472-473`). Whether the ingest still serves the image after a real restart or expiry, so that the Servers' build could reuse it, was not measured. Nor was how often a daemon restarts between a push and Cloud's look. No issue is filed.

## Keep the model in step

When a change alters a writer or reader of the build row's GitHub fields, or a step of the walk, change the model in the same PR. Examples: a new `github_finish` caller, a new end, a guard on `record()`, a change to `:206-218`'s order.

1. Update the function that copies the product function you changed, and the `reads_as` arm that prints it:
   - `checkStoreGithubBuild`: `BuildHandoff::check` and `At`.
   - `walkGithub` and `storeGithubBuildOpen`: `BuildHandoff::walk` and `Walk`.
   - `github_finish`, `settle` and `inspect_grant`: `BuildHandoff::finish`, `Finish` and `GrantRead`.
   - `github_end`: `State::github_end`.
   - `github_check_in`, `mint` and the old-mint skip: `BuildHandoff::pre_check`, `check_in` and `CheckIn`.
   - `github_report` and `recordStoreGithubBuildSteps`: the job's `Reporting`, `Finishing` and `Waking` arms.
   - `github_cancel` and `cancelStoreGithubBuilds`: `BuildHandoff::stop` and `Stop`.
   - the user's cancel: `BuildHandoff::user`.
2. If the change adds a guard, add a `Broken` flag that removes it, and a test naming the property it keeps.
3. If the change ships the fix, model it as the code writes it and make it the default. Then turn the finding tests into tests of a `Broken` flag that restores `timed_out`, and keep them, since their printed paths are the record of what the fix keeps out.
4. If the change adds a branch, add a `COVERAGE` row whose predicate requires the branch it names (`took`).
5. Every `State` field is read: `deployment`, `row`, `pushed` (by `finish`'s inspect), `cancelled` and `completed` (by `offers` and `runEnded`), `walk`, `job`, `again`, `user`, `crashes` (by `offers`), `limit_took` (by `STARTED_RUN_KEPT`), `limit_waited` (by the rows), `granted_after_cancel` (by `NO_GRANT_AFTER_CANCEL`), `grant_replaced` (by `ONE_GRANT_PER_RUN`) and `took` (by the rows). A field that is only written still splits states, so remove one that nothing reads.
6. Move the sync commit below forward only after you confirmed each covered function still reads as the model says.

## Coverage

Each model step is a copy of code at the sync commit. Reread these when a diff touches them:

- `dashboard/src/modules/config-store/store-github-builds.server.ts`: `checkStoreGithubBuild`, `storeGithubBuildOpen`, `runEnded`, `checkInStoreGithubBuild` and `recordStoreGithubBuildSteps`.
- `dashboard/src/modules/config-store/store-deployment.inngest.ts`: `walkGithub`, and the function's `retries` and `concurrency`.
- `dashboard/src/modules/config-store/config-store.server.ts`: `cancelDispatchedGithubRun`, `heldGithubBuild`, `cancelStoreGithubBuilds` and `afterWrite`'s cancel.
- `dashboard/src/routes/api/github/-webhook.handler.ts`: the `workflow_run` event.
- `core/crates/ployz/src/sdk/github_build.rs`: `github_check_in`, `mint`, `github_report`, `github_finish`, `settle`, `inspect_grant`, `grant_read` and `github_cancel`.
- `core/crates/ployz-store/src/build.rs`: `building_on`, `github_build`, `github_authorize`, `github_check_in`, `github_report`, `github_end` and `github_outstanding`.
- `core/crates/ployz-store/src/deployment/lifecycle.rs`: `cancel`.

Sync commit: `900cd6ee6ba198f3035ed407d44e7d727d3c874b`, the `main` this model was checked against, and every line cited above is at it. Code in the list above that changed after it has not been rechecked.
