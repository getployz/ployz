---
name: verify-caddy-load
description: Model-check the daemon's Caddy watch loop (loading the Ingress Projection into the Ingress Proxy through Caddy's admin or its Caddyfile, and the routed-address claim in PublishedRoutes) against Caddy restarting in place, being replaced, stopping, refusing a load, and the projection read failing, with the Stateright model in core/crates/ployz-verify. Use when a change touches core/crates/ployzd/src/ingress.rs (watch_caddy, apply_caddy, CaddyInput, CaddyCertificates, serves_nothing, newest_local_ingress, PublishedRoutes), core/crates/ployzd/src/ingress/caddy.rs (load, write_caddyfile), or what reads PublishedRoutes.addresses for ployz status or Deploy evidence.
---

# Verify the Caddy load

The model in `core/crates/ployz-verify/src/caddy_load.rs` explores every interleaving of the daemon's `watch_caddy` loop with the Ingress Proxy it drives. Each watcher step is one statement of `watch_caddy`, `apply_caddy`, `CaddyInput::loaded` or `CaddyCertificates::find_refused`, with every outcome the code handles: the admin found, down or failing to connect; `write_certificate_files`, `caddy::load`, `write_caddyfile` and `remove_stale_certificate_files` each succeeding or failing; a load refused by the admin and the probe that follows; the projection read failing. Caddy is an actor outside the watcher: it restarts in place under the same container id, is replaced by a new container, stops and starts, and a started Caddy serves the Caddyfile on disk. The replicated store changes the projection. Nothing calls the real daemon; the model is a hand copy, and drifts unless you keep it in step.

The property, in one sentence: **at rest, the published addresses are what Caddy serves.** When the watcher waits after a pass that read its inputs, `PublishedRoutes.addresses` is `None`, or it names the addresses of the config Caddy's process serves. The harm when it breaks: `ployz status` and Deploy evidence show every route confirmed while Caddy serves an older config, or one with a site left out, so a Deploy reads reachable when it is not.

## Run it

```
cd core && cargo test -p ployz-verify caddy_load -- --nocapture
```

Under a second warm; the whole crate takes about 40 s because of the Volume Runs model beside it. CI runs it in the workspace nextest, so a red model blocks the PR.

The run is green when all eight tests pass. Read them as three groups:

- **The findings.** `current_code_keeps_a_claim_a_restarted_caddy_no_longer_serves` and `current_code_leaves_the_pre_probe_claim_standing_when_the_next_read_fails` each pass by *finding* a counterexample on the code as it is, and print it. Each is red on code that closes its path. When a product fix lands, flip the test it closes: model the fix as the code wrote it and assert the property holds, and move the printed path into this skill as history.
- **Where the code already holds.** `current_code_holds_while_caddy_never_restarts_in_place_and_every_read_succeeds` runs the code as it is with those two events out, prints a state count and checks every property and every `sometimes`. A `sometimes` that is never reached fails here: the model can no longer reach that outcome, so a property passing beside it proves nothing.
- **The fix and the guards.** `a_claim_tied_to_the_caddyfile_and_written_on_every_exit_holds` runs the fix variant with every event in: the keep rule keeps a claim only while the Caddyfile holds what was recorded, and `Probe::Refused` writes the record before it re-derives. `the_cache_needs_a_running_caddy`, `a_reloaded_probe_must_clear_the_recording_caddy`, `the_keep_rule_needs_the_recording_container` and `the_record_needs_the_admins_confirmation` each drop one guard the code has (a `Broken` flag) and expect the property to fail. One of those going green-to-red means a guard no longer earns its keep, or the model lost the path that shows it.

## Read a counterexample

A failure prints the property name, the model's flags, and a numbered story. Each row is one statement in the code's words, or one Caddy or store event, with the claim, Caddy and the watcher's locals once it ran, for example:

```
 7. watcher: caddy::write_caddyfile fails through the admin: Err(AfterServing), the Caddyfile still holds P0 (apply_caddy)
    claim=None recorded_by=None | caddy X serves P1 file P0 | projection P1 last_input=None accepted=false refused=false
```

`claim` is `PublishedRoutes.addresses`: `None` is unconfirmed, `Some(nothing)` is the empty map a stopped Caddy gets, `Some(P1)` names the addresses of config `P1`. `recorded_by` is the container whose admin confirmed it. `caddy X serves P1 file P0` is the ingress container, what its process serves and what the Caddyfile on disk holds; `(unseen)` marks a Caddy event the watcher has had no pass to read. `projection` is what `next_pass` derives before `quarantine`. `last_input`, `accepted` and `refused` are the watcher's locals: `P1/X/admin` is a `CaddyInput` for projection `P1` and container `X` loaded through the admin, `file` one written without it, and the two flags are `CaddyCertificates` for the one pinned certificate `c`. `P1WithoutC` is `P1` with `c`'s material left out, which a probe loads, `loaded_only` writes while `c` is unaccepted, and `quarantine` derives once `c` is refused; without material the HTTPS site has no route, so its addresses differ from `P1`.

Write the story out in the watcher's words before deciding anything: which pass wrote the claim, through which path, and what Caddy served by then. A test that fails inside `assert_properties` prints the raw `Path[n]` of actions instead; to see the rows, call the module's `breaks` helper with that model in a scratch test, which prints the story.

If a picture would help, the pstack `/statechart` skill draws how a model moves between states and tells each shortest failing path in plain words. Refer to it by name; its commands change between releases.

## Real bug or model error

A counterexample is a **lead**, not a verdict. Confirm each step of the story against the function it names before calling it a bug. Each `Pc` and `Action` variant's doc comment names its file and function, and `reads_as` prints the statement each step copies.

- If a step does something the code cannot, fix the model and say so. The usual suspects: a Caddy event the model lets land where the code could see it (`unseen` and `at_rest` decide which states the property judges), an outcome `apply_caddy` cannot return from that call (`BeforeServing` against `AfterServing`, and whether the Caddyfile was written), and what `find_refused` leaves in `accepted` and `refused` on each of its exits.
- If every step matches the code, it is a finding. Report it in plain words with the path. Keep the property and the model as they are; weakening either to go green hides the bug.

The two findings this model was built on, confirmed step by step at the sync commit below:

1. **The claim outlives an in-place restart (#2787).** Caddy loads `P1` through its admin, then `write_caddyfile` fails, so `apply_caddy` returns `AfterServing` and the Caddyfile still holds `P0`. The `(AfterServing, true)` arm accepts the certificates, and the record write claims `P1` with `recorded_by` the container. Caddy restarts in place under the same id and serves the Caddyfile, `P0`. The watcher never sees the restart, since the replicated observation carries no start time. The next pass fails before serving (here `write_certificate_files`, and `caddy::load` failing short of the admin reaches the same arm), and the keep rule finds `recorded_by == caddy` and leaves the claim at `P1`. The comment above the keep rule admits this path; the model shows it needs only one failed write and one restart.
2. **A refused probe skips the record write.** A load is refused and `find_refused` quarantines `c`: `Probe::Refused` clears `recorded_by` and `continue`s to the top of the loop without the record write, while Caddy serves the probe's `P1WithoutC`. The next read fails, and its `Err` arm only logs, so the watcher waits on the claim from the pass before. In the printed path that claim is `P1`, written by an `AfterServing` pass; any earlier claim does. Each read failure extends the window by one debounce.

## A break that stays green

Guards overlap, so removing one can leave every property holding. Before calling a guard dead, rerun with a neighbouring `Broken` flag set too, or under the other `Fix` part. `the_keep_rule_needs_the_recording_container` only shows the container check's worth with the fix's Caddyfile check out and restarts off: with the Caddyfile check in, a new container that started from a stale file is already refused a stale claim, and with restarts on, finding 1 breaks the property first. That is why the Caddyfile check is not the whole guard: the keep rule must still name the container, or a replacement inherits the old container's claim on its first refusal.

## Keep the model in step

When a change intentionally alters the loop (a new guard, a new statement, a reordered call, a new `ApplyError` shape, a new `Probe` arm), change the model in the same PR:

1. Update the matching `Pc` variant, `Action`, or `step`'s arm, citing the file and function it copies, and the `reads_as` line that prints it.
2. If the change adds a guard, add a `Broken` flag that removes it and a `breaks` test naming the property it keeps. A guard with no failing variant is untested.
3. If the change fixes a finding, flip its test as described under Run it. Do not delete it: its printed path is the record of what the fix keeps out.
4. If a property's meaning changes, change its text and say why in the PR body. Narrow a property only when the code is right and the property was wrong about the design.
5. Move the sync commit below forward only after you confirmed each covered function still reads as the model says.

## Bounds and blind spots

Bounds: one Machine that takes ingress, with one ingress container at a time, `X`, replaced at most once by `Y`; two projections, `P0` with one HTTP site and `P1` with that site moved and an HTTPS site pinning one certificate `c`; Caddy events (restart in place, replace, stop, start) only while the watcher waits, and one per wait, so no restart lands mid-pass; the projection changing only while the watcher waits. The property judges a state only at rest: the watcher waits, and no Caddy event has landed since the pass that put it there. A window between a Caddy event and the pass that reads it is not the watcher's to close, so the model does not count it.

Out of scope: a restart landing between `caddy::load` and `write_caddyfile`, which leaves the same stale claim with no failure at all and which the bounds exclude; the route acks and `PublishedRoutes.routes`; `down` and the kept route; two pinned certificates; the role turned off and a Machine with no ingress container; the admin confirming a load that Caddy then drops. A green model says nothing about them; cover them with the watcher tests in `core/crates/ployzd/src/caddy_tests/watcher.rs`.

## Coverage

Each model step is a copy of code at the sync commit. Reread these when a diff touches them:

- `core/crates/ployzd/src/ingress.rs`: `watch_caddy` whole, with the `loaded` cache, `connect` and its `Err` arm, the equal-input skip, the `Probe` arms, the `(applied, admin)` match, `refused`, the keep rule, the record write, the retry wait, the read's `Err` arm and the debounce wait; `apply_caddy` and each `ApplyError` it returns; `CaddyInput::loaded`; `CaddyCertificates::loaded_only`, `quarantine`, `accept`, `find_refused`; `Probe`; `serves_nothing`; `newest_local_ingress`; `routed_addresses`; `PublishedRoutes`.
- `core/crates/ployzd/src/ingress/caddy.rs`: `load` and when it goes through the admin, `write_caddyfile`.
- `core/crates/ployzd/src/caddy_tests/watcher.rs`: `routed_addresses_stand_while_the_caddy_that_loaded_them_refuses_a_new_load`, the test of the keep rule.

Sync commit: `1e3ada53e2b1a9259ee577ddb270b117b1a3b001`, the `main` this model was checked against. Code in the list above that changed after it has not been rechecked.
