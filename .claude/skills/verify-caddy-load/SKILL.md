---
name: verify-caddy-load
description: Model-check the daemon's Caddy watch loop (loading the Ingress Projection into the Ingress Proxy through Caddy's admin or its Caddyfile, and the routed-address claim in PublishedRoutes) against Caddy restarting in place, being replaced, stopping, refusing a load, a load that takes while its response is lost, a Caddyfile write that fails after replacing the file, and the projection read failing, with the Stateright model in core/crates/ployz-verify. Use when a change touches core/crates/ployzd/src/ingress.rs (watch_caddy, apply_caddy, CaddyInput, CaddyCertificates, serves_nothing, newest_local_ingress, PublishedRoutes), core/crates/ployzd/src/ingress/caddy.rs (load, write_caddyfile, AdminClient), core/crates/ployzd/src/filesystem.rs (atomic_write), or what reads PublishedRoutes.addresses for ployz status or Deploy evidence.
---

# Verify the Caddy load

The model in `core/crates/ployz-verify/src/caddy_load.rs` explores every interleaving of the daemon's `watch_caddy` loop with the Ingress Proxy it drives. Each watcher step is one statement of `watch_caddy`, `apply_caddy`, `CaddyInput::loaded` or `CaddyCertificates::find_refused`, with every outcome the code handles: the admin found, down or failing to connect; `write_certificate_files` and `remove_stale_certificate_files` each succeeding or failing; `caddy::load` succeeding, refused, failing short of Caddy, or taking while its response is lost; `write_caddyfile` succeeding, failing before its rename, or failing after it; a load refused by the admin and the probe that follows; the projection read failing. Caddy is an actor outside the watcher: it restarts in place under the same container id, is replaced by a new container, stops and starts, and a started Caddy serves the Caddyfile on disk. The replicated store changes the projection. Nothing calls the real daemon; the model is a hand copy, and drifts unless you keep it in step.

The claim is `PublishedRoutes.addresses`: the routed addresses that `ployz status` and Deploy evidence read as what the Ingress Proxy serves. Two properties about it, judged only at rest:

- `CLAIM_SERVED`: **at rest, the published addresses are what Caddy serves.** The claim is `None`, or it names the addresses of the config Caddy's process serves. It fails on an over-claim (`Some(P1)` while Caddy serves `P0`) and on an under-claim (`Some(nothing)` while Caddy serves routes).
- `CLAIM_CONFIRMED`: **at rest, a claim of addresses names the Caddy that confirmed it.** A `Some(config)` claim has a `recorded_by`.

At rest means the watcher waits after a pass that read its inputs, in the debounce wait or in the retry sleep after a failed apply, and no Caddy event has landed since. The harm when `CLAIM_SERVED` breaks: `ployz status` and Deploy evidence show every route confirmed while Caddy serves an older config, or one with a site left out, so a Deploy reads reachable when it is not. What keeps each stale claim standing on real code, and its first chance to reconcile, is under [How long a stale claim stands](#how-long-a-stale-claim-stands).

## Run it

```
cd core && cargo test -p ployz-verify caddy_load -- --nocapture --test-threads=1
```

Under ten seconds warm; the whole crate takes about 40 s because of the Volume Runs model beside it. One test thread keeps each story in order under its test's name, which prints in brackets above it. CI runs it in the workspace nextest, so a red model blocks the PR.

The run is green when all eleven tests pass. Read them as three groups:

- **The findings.** `current_code_keeps_a_claim_a_restarted_caddy_no_longer_serves`, `current_code_leaves_the_pre_probe_claim_standing_when_the_next_read_fails` and `current_code_keeps_a_claim_after_a_load_that_lost_its_response` each pass by *finding* a counterexample on the code as it is, and print it. Each turns on one event (`restarts`, `reads_fail`, `loads_lost`) over the `QUIET` bounds, so the printed path needs that event and nothing else the code does not already answer for. Each is red on code that closes its path. When a product fix lands, flip the test it closes: model the fix as the code wrote it and assert the property holds, and move the printed path into this skill as history.
- **Where the code already holds.** `current_code_holds_while_caddy_never_restarts_and_every_read_and_load_answers` runs the code as it is under `QUIET`: no restart in place, every read succeeds, every load answers. Caddy still stops, starts and is replaced; loads are refused and probed; writes fail before and after the rename; the projection changes. It prints a state count and checks every property and every `sometimes`. A `sometimes` that is never reached fails here: the model can no longer reach that outcome, so a property passing beside it proves nothing. Each `sometimes` is one row of the `COVERAGE` table, naming one call outcome, record or Caddy event, and its predicate reads that event from the state that recorded it: `last`, the action that led there; the record `Pc`; or the unseen Caddy event. `every_sometimes_needs_what_it_names` runs with every bound on, then takes each row's `omit` out of the model (the actions, the record, the event, or the kept record) and expects the row unreached. A row reached without its cut is vacuous, its predicate looser than its name, and that test is red until the predicate names the event. The cut proves something only where the predicate says more than the cut takes out: a row reading `last == Some(X)` with `omit: Actions([X])`, a record `Pc` with `Omit::Record` of it, or an unseen event with `Omit::Event` of it, cannot be reached without its cut, and most rows are of that shape; for them the test records that the outcome stays reachable. The cut tests the eight rows whose predicate adds a condition: the admin load that claims the projection, the before-rename failure in the `AfterServing` arm, the candidate refused with `c` quarantined, the stale-file refusal, the failed read ending at the wait, the restarted Caddy serving an older Caddyfile, the new container confirming, and the stopped Caddy claiming nothing.
- **The fix, what it leaves open, and the guards.** `a_claim_tied_to_the_caddyfile_and_written_on_every_exit_holds` runs the fix variant with restarts in place and read failures back in: the keep rule keeps a claim only while the Caddyfile holds what was recorded, and `Probe::Refused` writes the record before it re-derives. `the_fix_does_not_close_a_restart_between_the_load_and_the_caddyfile_write` passes by finding the path the fix leaves open (below). `the_cache_needs_a_running_caddy`, `a_reloaded_probe_must_clear_the_recording_caddy`, `the_keep_rule_needs_the_recording_container` and `the_record_needs_the_admins_confirmation` each drop one guard the code has (a `Broken` flag) and expect the property to fail. One of those going green-to-red means a guard no longer earns its keep, or the model lost the path that shows it.

## Read a counterexample

A failure prints the test name, the property name, the model's flags, and a numbered story cut at the first state that breaks the property, so the last row is the broken state. Each row is one statement in the code's words, or one Caddy or store event, with the claim, Caddy and the watcher's locals once it ran, for example:

```
 7. watcher: caddy::write_caddyfile fails through the admin before the rename: Err(AfterServing), the Caddyfile still holds P0 (apply_caddy, filesystem::atomic_write)
    claim=None recorded_by=None | caddy X serves P1 file P0 | projection P1 last_input=None accepted=false refused=false
```

`claim` is the claim: `None` is unconfirmed, `Some(nothing)` is the empty map a stopped Caddy gets, `Some(P1)` names the addresses of config `P1`. `recorded_by` is the container whose admin confirmed it. `caddy X serves P1 file P0` is the ingress container, what its process serves and what the Caddyfile on disk holds; `(unseen: Restarted)` marks a Caddy event the watcher has had no pass to read. `projection` is what `next_pass` derives before `quarantine`. `last_input`, `accepted` and `refused` are the watcher's locals: `P1/X/admin` is a `CaddyInput` for projection `P1` and container `X` loaded through the admin, `file` one written without it, and the two flags are `CaddyCertificates` for the one pinned certificate `c`. `P1WithoutC` is `P1` with `c`'s material left out, which a probe loads, `loaded_only` writes while `c` is unaccepted, and `quarantine` derives once `c` is refused; without material the HTTPS site has no route, so its addresses differ from `P1`.

Write the story out in the watcher's words before deciding anything: which pass wrote the claim, through which path, and what Caddy served by then. A test that fails inside `assert_properties` prints the raw `Path[n]` of actions instead; to see the rows, call the module's `breaks` helper with that model in a scratch test, which prints the story.

If a picture would help, the pstack `/statechart` skill draws how a model moves between states and tells each shortest failing path in plain words. Refer to it by name; its commands change between releases.

## Real bug or model error

A counterexample is a **lead**, not a verdict. Confirm each step of the story against the function it names before calling it a bug. Each `Pc` and `Action` variant's doc comment names its file and function, and `reads_as` prints the statement each step copies.

- If a step does something the code cannot, fix the model and say so. The usual suspects: a Caddy event the model lets land where the code could see it (`unseen` and `at_rest` decide which states the property judges), an outcome `apply_caddy` cannot return from that call (`BeforeServing` against `AfterServing`), what a failed call left behind (a `Load::Lost` has Caddy serving the config, a `Write::FailedAfterReplacing` has the file replaced), and what `find_refused` leaves in `accepted` and `refused` on each of its exits.
- If every step matches the code, it is a finding. Report it in plain words with the path. Keep the property and the model as they are; weakening either to go green hides the bug.

The three findings this model was built on, confirmed step by step at the sync commit below, each with the path its test prints:

1. **The claim outlives an in-place restart (#2787).** Caddy loads `P1` through its admin, then `write_caddyfile` fails before its rename, so `apply_caddy` returns `AfterServing` and the Caddyfile still holds `P0`. The `(AfterServing, true)` arm accepts the certificates, and the record write claims `P1` with `recorded_by` the container. Caddy restarts in place under the same id and serves the Caddyfile, `P0`. The watcher never sees the restart, since the replicated observation carries no start time. The next pass fails before serving (here `write_certificate_files`; `caddy::load` failing short of Caddy reaches the same arm), and the keep rule finds `recorded_by == caddy` and leaves the claim at `P1`. The comment above the keep rule admits this path; the model shows it needs only one failed write and one restart. The path is 18 rows.
2. **A refused probe skips the record write.** Caddy `X` confirms `P1` after a write that failed before the rename, so the Caddyfile holds `P0`, and a new container `Y` replaces it and serves that file. The next pass loads `P1`; the admin refuses it, and `find_refused` runs: `loaded_certificates` shows no `c`, the load without `c` takes, so Caddy serves `P1WithoutC`, and the candidate with `c` is refused twice, so `c` is quarantined. `Probe::Refused` clears `recorded_by` and `continue`s to the top of the loop without the record write. The next read fails, and its `Err` arm only logs, so the watcher waits on the claim from the pass before, `Some(P1)`, while Caddy serves `P1WithoutC`. Any earlier claim does. The path is 22 rows. The replacement is one way for `loaded_certificates` to come back without `c`; a confirmed `P0` with the projection changed to `P1` is the other, at the same length and on container `X`.
3. **A lost load keeps the old claim.** Caddy confirms `P1` and the projection changes to `P0`. `caddy::load(P0)` reaches Caddy, which applies it, and the response is lost: `AdminClient::post` sends the body and awaits the response under `ADMIN_TIMEOUT`, so a timeout or a dropped connection comes back as `caddy::Error::Http`. `apply_caddy` returns `BeforeServing`, which the code reads as Caddy unchanged, so no probe runs, `refused` is true, and the keep rule finds `recorded_by == caddy` and leaves the claim at `P1` while Caddy serves `P0`. No restart and no failed read is needed. The path on main gets its claim from a `write_caddyfile` that failed before the rename, 18 actions in 19 rows. The fix variant keeps the claim too when the Caddyfile holds what was recorded, so its path writes the file, 20 rows, and the test asserts the same over-claim with `claim_is_written`.

## What the fix leaves open

The fix variant (`Fix::BOTH`) closes findings 1 and 2 and holds with restarts in place and read failures on. It does not close #2787 in full, and E-20 must not claim that it does:

- **A restart between `caddy::load` and `write_caddyfile`** (`restarts_mid_apply`). Caddy loads `P1` through its admin, restarts in place and serves the Caddyfile, `P0`; the write then lands `P1`, `remove_stale_certificate_files` succeeds, and the pass claims `P1` with `last_input` set. Every later pass hits the `loaded` cache (`!no_caddy && last_input.loaded(..)`), which skips the admin while Caddy runs and the last load matches, so no retry, debounce or resnapshot pass corrects it; its first cache miss is the next projection or ingress container change, or Caddy stopping, and that pass can keep the claim too (the fix's keep rule keeps it when the write fails before serving with the container unchanged and the Caddyfile holding what was recorded). `the_fix_does_not_close_a_restart_between_the_load_and_the_caddyfile_write` prints this path (15 rows) under `Fix::BOTH`. The bound allows one such restart per apply, only through the admin, and only while no other Caddy event is unseen.
- **A lost load** (`loads_lost`), finding 3 above. For E-20: only `admin.load` inside `caddy::load` (`caddy.rs`) leaves Caddy's state unknown on an error, since `Http` from `adapt`, and every `Io` and `Json` error, come before Caddy changes; treating `BeforeServing` as unknown must split on which call failed, not on the error type. `find_refused`'s load without the suspects maps any error to `Probe::Unchanged` (`ingress.rs`, the `is_err()` guard before the candidates), a second site of the same finding. A lost candidate load is already safe: it reads as `Probe::Reloaded`, which clears `recorded_by`.

Each needs its own remedy. The mid-apply restart needs the watcher to see the restart, a start time in the observation, since nothing in its locals changes. The lost load has no restart and no start time to see: it needs Caddy's config read back through the admin, or a probe, before the record write, or the uncertain claim dropped.

## How long a stale claim stands

The model judges the retry sleep and the debounce wait as at rest, so each path above counts as a break. On real code one rule holds: a stale claim ends only when a pass's record write (`route_publishes.send_if_modified` in `watch_caddy`) replaces or withdraws it. Nothing else touches `PublishedRoutes.addresses`. On every path below, the next pass can fail in the same way and keep the claim, so the last column is a chance, not a recovery.

| path | what keeps it standing | first chance to reconcile |
|---|---|---|
| finding 1, the restart in place | each pass through the admin fails before serving (`write_certificate_files`, or `caddy::load` short of Caddy), so the keep rule `refused && recorded_by == caddy` skips the record write | `wait_before_retry` after the failed pass, `WATCH_RETRY` (1 s), then the next pass |
| finding 2, the refused probe | each read fails: the `Err` arm logs and goes to `wait_for_debounced_change`, with no record write | the next read: a container or certificate change, or `WATCH_RESNAPSHOT` (5 s) in `wait_for_debounced_change` |
| finding 3, the lost load | each retried load loses its response again (`BeforeServing(Http)`, then the keep rule as in finding 1) | `wait_before_retry`, `WATCH_RETRY` (1 s), then the next pass |
| the mid-apply restart, under the fix | `CaddyInput::loaded` holds: `through_admin`, the same projection and the same container, with `!no_caddy`. No retry, debounce or resnapshot pass corrects it, since each hits the cache | the next projection or ingress container change, or Caddy stopping: each misses the cache, and the pass goes through the admin or claims for a stopped Caddy |

## A break that stays green

Guards overlap, so removing one can leave every property holding. Before calling a guard dead, rerun with a neighbouring `Broken` flag set too, or under the other `Fix` part. `the_keep_rule_needs_the_recording_container` only shows the container check's worth with the fix's Caddyfile check out and restarts off: with the Caddyfile check in, a new container that started from a stale file is already refused a stale claim, and with restarts on, finding 1 breaks the property first. That is why the Caddyfile check is not the whole guard: the keep rule must still name the container, or a replacement inherits the old container's claim on its first refusal.

## Keep the model in step

When a change intentionally alters the loop (a new guard, a new statement, a reordered call, a new `ApplyError` shape, a new `Probe` arm), change the model in the same PR:

1. Update the matching `Pc` variant, `Action`, or `step`'s arm, citing the file and function it copies, and the `reads_as` line that prints it.
2. If the change adds a guard, add a `Broken` flag that removes it and a `breaks` test naming the property it keeps. A guard with no failing variant is untested.
3. If the change fixes a finding, flip its test as described under Run it. Do not delete it: its printed path is the record of what the fix keeps out.
4. If a property's meaning changes, change its text and say why in the PR body. Narrow a property only when the code is right and the property was wrong about the design.
5. If the change adds an outcome, add a `COVERAGE` row naming it, with the `omit` that takes it out. `every_sometimes_needs_what_it_names` fails the row while any other path reaches its predicate.
6. Move the sync commit below forward only after you confirmed each covered function still reads as the model says.

## Bounds and blind spots

Bounds: one Machine that takes ingress, with one ingress container at a time, `X`, replaced at most once by `Y`; two projections, `P0` with one HTTP site and `P1` with that site moved and an HTTPS site pinning one certificate `c`; Caddy events (restart in place, replace, stop, start) only while the watcher waits, and one per wait, except the one mid-apply restart `restarts_mid_apply` allows; the projection changing only while the watcher waits. Each `CaddyLoad` field is a bound: `restarts`, `restarts_mid_apply`, `reads_fail` and `loads_lost` turn an event on over `QUIET`. The property judges a state only at rest: the watcher waits, and no Caddy event has landed since the pass that put it there. A window between a Caddy event and the pass that reads it is not the watcher's to close, so the model does not count it.

Out of scope: the lag of the replicated observation behind a Caddy event, since `Read(Ok)` exposes a replacement at once while `newest_local_ingress` reads an observation that can lag it; the route acks and `PublishedRoutes.routes`; `down` and the kept route; two pinned certificates; the role turned off and a Machine with no ingress container; the admin confirming a load that Caddy then drops; Caddy events during a pass other than the one mid-apply restart; the equal-input skip through the admin, which the code reaches only when the store says no Caddy while the admin socket still answers (that lag again), so the model reaches it only without the admin. A green model says nothing about them; cover them with the watcher tests in `core/crates/ployzd/src/caddy_tests/watcher.rs` (`cargo nextest run -p ployzd --lib ingress::caddy::tests::watcher`).

## Coverage

Each model step is a copy of code at the sync commit. Reread these when a diff touches them:

- `core/crates/ployzd/src/ingress.rs`: `watch_caddy` whole, with the `loaded` cache, `connect` and its `Err` arm, the equal-input skip, the `Probe` arms, the `(applied, admin)` match, `refused`, the keep rule, the record write, the retry wait, the read's `Err` arm and the debounce wait; `apply_caddy` and each `ApplyError` it returns; `CaddyInput::loaded`; `CaddyCertificates::loaded_only`, `quarantine`, `accept`, `find_refused`; `Probe`; `serves_nothing`; `newest_local_ingress`; `routed_addresses`; `PublishedRoutes`.
- `core/crates/ployzd/src/ingress/caddy.rs`: `load` and when it goes through the admin, `write_caddyfile` and what runs after its `atomic_write` (`set_ployz_group`), `AdminClient::post` and `ADMIN_TIMEOUT`, `Error`.
- `core/crates/ployzd/src/filesystem.rs`: `atomic_write`, which renames into place and then syncs the directory, so a failure after the rename leaves the new file.
- `core/crates/ployzd/src/caddy_tests/watcher.rs`: `routed_addresses_stand_while_the_caddy_that_loaded_them_refuses_a_new_load`, the test of the keep rule.

Sync commit: `1e3ada53e2b1a9259ee577ddb270b117b1a3b001`, the `main` this model was checked against. Code in the list above that changed after it has not been rechecked.
