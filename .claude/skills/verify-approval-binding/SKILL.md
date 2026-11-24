---
name: verify-approval-binding
description: Model-check that a person's yes (a carried approval digest, a Drain's yes, a Device's access on a Machine) is weighed against the plan and the keys as they stand when its effect runs, with the Stateright model in core/crates/ployz-verify/src/approval_binding.rs. Use when a change touches a carried yes or a shortcut past one: Approval::carried in ployz-store's trusted.rs; Gate, approve, approve_retry, approve_shut_down, approve_cancel, move_asks or approve_move in ployz-store's review/approval.rs; publish's early return in review/publish.rs; gate, guard, remove or cancel in removal/mod.rs; shut_down or ask_taking_off in removal/due.rs; the Admit::Retry arm in deployment/admit.rs; forgettable or cancel in deployment/lifecycle.rs; head's token in deployment/history.rs; handleConfigRequest in dashboard/src/routes/api/config/-config.handler.ts; gateOperation in dashboard/src/modules/operations/gate.server.ts; drainServer in operations.server.ts, stillCovered and executeDrainOnce in machines/server-drain.server.ts, drainPlan in machines/server-operations.server.ts, planDrainVolumes or admitDrainVolume in machines/server-drain-volumes.server.ts, or dataCheck in machines/server-drain-volumes.ts; the Engine drain's preflight in ployz/src/drain.rs, retire_copies in ployz/src/global_slot/lifecycle.rs, retire or wait_unlisted in ployz/src/deploy/cleanup.rs, proxy_routes in ployz-core's service/serving.rs, Reach in ployzd's reach.rs, or last_in in ployzd's machine/local_machine/rotation.rs; or ployzd's admission turns (turns.rs finish_*, exclusive_turn, admit_installation, require_management_access) and the work that takes exclusive_turn itself in machine/local_machine/container.rs.
---

# Verify approval binding

One fault has been fixed again and again across Engine and Cloud: a yes checked against a different plan, or at a different moment, than the write it allowed. The model in `core/crates/ployz-verify/src/approval_binding.rs` replays each of those fixes and checks every gate against one property:

**Every effect runs the plan a valid yes was given for.** A request that carries a yes writes exactly the plan the person saw when they said yes, and only while the yes still counts: never an agent's; in a Protected Environment, never a member's own where its protection asks an owner or admin; on a Machine, only from a key it accepts when the effect runs. The harm when it breaks: something runs under a person's name that they never saw, or after their Device was removed.

The model is a hand copy of the gates. Nothing calls the real Store, Cloud or daemon, so it drifts unless you keep it in step. Its module doc names every actor and the function each step copies; read it before changing anything.

## Run it

```
cd core && cargo nextest run -p ployz-verify approval_binding --no-capture
```

Under a second. CI runs every `ployz-verify` model in the `rust-tests (cli-*)` legs of `.github/workflows/ci.yml`, which `scripts/ci.py` selects for any change under `core/crates/`. A change only under `dashboard/` does not run it: the model reads no dashboard code, so mirror a dashboard change into the model yourself and run it.

Green is 24 passing tests, in three groups:

- **The code holds.** `the_code_binds_every_yes_to_its_plan_and_moment` checks every command as it ships (Save, Deploy, Retry, RemovalRetry, ShutDown, Remove, Cancel, Move, Drain, a turned Machine route), prints each state count, and fails on any unreached `sometimes`: a branch the model can no longer reach makes a passing property meaningless. It leaves out the open paths below: the installed route, and the changes `open_paths` adds (a Cancel's Deployment starting, B stopping unseen, the proxies restarting).
- **The past bugs and guards.** Each test sets one `Broken` flag that restores the code before a fix, or removes a guard, and expects a counterexample. If one goes green, the guard stopped mattering or the model lost the path that shows it.
- **The open paths.** Six tests pass because the shipped code breaks the property (below). When one is fixed, its test goes red: move its change into the shipped model and give the fix a `Broken` flag and a breaking test, as the others have.

| Commit | `Broken` flag | Command |
|---|---|---|
| f06d2a6351 | `cloud_calls_twice`, `approve_skips_yes` | Save, Deploy |
| 0f29e0d3f3 | `empty_save_skips_yes`, `forget_unbound`, `cancel_skips_unless_theirs` | Save, ShutDown, RemovalRetry, Cancel |
| 8836d08f2f | `retry_skips_yes`, `protector_retry_skips_yes`, `forget_skips_yes`, `shutdown_skips_yes`, `remove_skips_yes`, `cancel_skips_unprotected`, `move_skips_yes` | Retry, ShutDown, RemovalRetry, Remove, Cancel, Move |
| 1c93248b9b | `agent_yes_unrefused`, `gate_skips_yes`, `drain_runs_harmless` | Save, Drain |
| bad55e1aed | `drain_runs_gone` | Drain |
| 273b77fabc | `turn_trusts_arrival` | Machine, turned route |
| guard | `member_yes_answers_protected` | Save |
| guard | `retire_skips_last_in` | Drain |
| guard | `retire_skips_unlisting` (removes as `retire_unconfirmed` does) | Drain |

273b77fabc's teardown statements were removed in cfd221baf1, so its flag replays the shape (checked at arrival, run under a later turn) on what authorizes a call today, the caller's management access. Its other half, enforcement turned on while a call waited, cannot be expressed: every daemon has enforced since a03ef88eea.

## Read a counterexample

Each failing path prints as a numbered story, one step per row in the code's words, then the state:

```
 1. editor: the plan changes to one that destroys nothing
    plan=Harmless key=accepted exec=Ready wrote=nothing
 2. executor: handleConfigRequest sends the yes into the write, approve_retry weighs it there (under Broken), and it runs
    plan=Harmless key=accepted exec=Ran wrote=Harmless under a yes that counts
```

`plan` is the plan as it stands, and `Asked` is the one the person said yes to. `key` is whether the Machine accepts the caller. A Drain's rows add `B`, global G's other copy: whether its owner last published it running (what Cloud's `drainPlan` and the proxies read), its rotation (`In`, `Out`, or `Down` for its Machine, what `last_in` and the proxies read), whether it answers traffic, and the Drain step each change landed before (`stopped`, `left`, `failed silently`); `proxies`, whether they restarted after A went Out and so hold no route from before; `retirement`, what became of G's copy A on the drained Server (`Retired`; `NotRetired`, the `LastIn` guard refused; `NotUnlisted`, a proxy kept routing A so `retire` returned `NotConfirmed`); `data`, what `dataCheck` reads of the Service left for its Volume; and `admission`, what `admitDrainVolume` did with a planned move. `wrote` is the effect and whether its yes counted when it ran. A break is any `wrote` other than `Asked under a yes that counts`. `assert_properties` prints raw actions instead; to read one as a story, call `breaks_ending` with that model in a scratch test.

## Real bug or model error

A counterexample is a lead. Check each row against the function it names (the `Write`, `Route` and `Action` doc comments give file and function) before calling it a bug.

- If a step does what the code cannot, fix the model and say so. The usual suspects are `store_gate`'s per-write shortcuts, `protection_asks` (which writes a Protected Environment's protection asks about), `ApprovalBinding::edits` (which plans the editor can reach, and when), `copy_changes` and `data_changes` (when B's copy and the Volume read change), `Plan::digest` (which changes the code's digest binds), and the Store write being one atomic step: it runs under the Environment's lock, which the editor's write takes too. A Drain is not one step: Cloud compares, the Engine reads again and each global's `LastIn` guard decides, the copy goes Out, `wait_unlisted` waits for every proxy to drop it (a proxy that restarts in between holds no route to keep), it is removed, Cloud plans the Volume moves from a new frame, and admission admits or refuses each. A retry runs the compare again, so a change made during the dead attempt counts as before the retry's compare.
- If every step matches, it is a finding. Report it with the story, scored per AGENTS.md. Keep the property: weakening it to go green hides the bug.

## The open paths

Each is scored per AGENTS.md, filed when a user can reach it, and left in the code. Its test passes while the code breaks the property, except the last row, which the model does not cover.

| Issue | Test | What runs | F | S |
|---|---|---|---|---|
| #4553 | `an_installed_step_runs_for_a_key_revoked_while_it_drained` | `finish_installed` checks the caller's management access when the call arrives; its work drains, takes `exclusive_turn`, which checks nothing, and stops or removes a Container for a Device `set_device` removed meanwhile. Removing a Container can delete its only anonymous-volume data. Fixed by #4558 (open), which checks under each turn the work takes. | 1 | 10 |
| #4555 | `a_cancel_s_yes_while_queued_stops_a_started_deployment` | A carried-digest case. `approve_cancel` binds the Deployment and the review version; Head's token counts only ended Deployments, so the runner's claim moves neither, and a carried yes to take a queued Deployment out of the queue stops it partway once it runs (`Plan::Started`). No end-to-end approval path is shown: an authorized caller's cancel skips the first ask, and a protected member's gets a refusal. | 1 | 4 |
| none | `a_retirement_removes_a_global_s_copy_beside_a_crash_looping_copy_read_running` | B crash-loops under `unless-stopped` with no healthcheck, from before the ask. Both of Cloud's reads (`drainPlan` at the gate and at `stillCovered`, `server-operations.server.ts:92`) land on a moment it reads Running, during a long failed start or from a lagging observation. `last_in` counts B In, the proxies read a Serving Container and drop A, and `retire` removes A. B exits again before it serves, and G stays down. S9 meets the fix bar: #4567 makes the Drain refuse any user Global. | 1 until #4567 | 9 |
| #4556 | `a_retirement_counts_a_global_s_other_copy_that_stopped_unseen_before_its_guard` | G's Container on B exits after Cloud's compare and before the drained Server's guard reads, and its exit has not replicated when the proxies drop A. B's daemon still answers In, not leaving, so `last_in` counts it (had the proxies already dropped B, they would route A alone and `last_in` would refuse); the proxies read B Running and route it alone; `retire` removes A. Had the proxies read the exit, they would keep A routed and the Drain would report `NotRetired` (`UNLISTING_KEEPS`). | 1 until #4567 | 6 |
| #4556 | `a_retirement_removes_a_global_s_copy_beside_one_that_stopped_unseen_after_its_guard` | `last_in` counted B. B's Container exits before the proxies drop A, and its exit has not replicated by then. | 1 until #4567 | 6 |
| #4556 | `a_retirement_removes_a_global_s_copy_once_a_restarted_proxy_drops_its_route` | `last_in` counted B and A went Out. B stops and its owner publishes it, or B's Machine goes Down. Every proxy `wait_unlisted` reads restarts before it reads them (one, in a small Cluster): `Reach::new` (`ployzd/src/reach.rs`) holds no route, so `next_route` gets no `current` and routes what serves now, or B from its Down Machine's stale row, never the Out A. `wait_unlisted` passes and `retire` removes A, though the proxies saw B stopped or Down. | 1 until #4567 | 6 |
| none | not modelled | A stop-first rollout of a Service with 2 replicas, live today. `last_in` lets the healthy copy go Out while the other crash-loops, counting it In from its rotation alone, so the Service has no serving copy until the rollout's new copy is healthy. The gap heals within the deploy, and no yes is weighed in it, so it sits outside this property. F×S = 6, under the fix bar. | 2 | 3 |

The four global rows share one fault: nothing reads whether B answers traffic. The first three read only what B's owner last published. The fourth reads the proxies' acks, and a proxy that restarted forgot that it was keeping A. A Full caller can reach all four. The Machine API admits a resolved Global spec from a Full caller (`ployzd` `admission.rs`, `Scope::Full`). The Engine's Drain classifies existing Containers by their observed spec (`observed_global_slot`, `ployz-core/src/service.rs`). Compose accepted `deploy.mode: global` up to v0.1.2-beta.41. A Drain never retires a system Global, because `DrainScope::includes` (`ployz/src/drain.rs:63`) drops `ployz-system`. A real cluster confirmed both halves of the mechanism: a copy stays In after its Container exits, and `last_in` reads no runtime state.

Each F is 1 until #4567 merges. #4567 makes the Drain refuse any user Global, before it changes anything, when any copy of a Service it would act on is Global, so no Drain retires one, a Global left from Compose up to v0.1.2-beta.41 included. That takes every row to F0. A `last_in` that counts only serving copies would close the crash loop and the stops before the guard (inferred). It would not close a stop after the guard, with or without a restart. S9 is a crash loop that never serves. S6 is an exit Docker restarts. The model keeps `serving` apart from what Cloud and the proxies read. `open_paths` lets B stop answering alone (`CopyFails`), and lets the proxies restart once A is Out (`ProxiesRestart`). #4556's S6 covers the outage only, G down until B serves again. Today's retirement still passes `AnonymousVolumes::Remove`, so removing A also deletes its anonymous volumes until Unit I lands. A B that fails after the proxies dropped A is the plan the yes named: G runs only on B.

`open(command)` in the tests turns on `open_paths`. `the_code_binds_every_yes_to_its_plan_and_moment` runs without it. There, a Drain still meets every change the code reads, until the proxies drop A:
- B's Container stops before Cloud's compare: `drainPlan` reads it and the compare refuses (`COPY_STOP_CHANGED`).
- It stops after the compare or after the guard, or B's Machine goes Down after the guard: a proxy that has not restarted keeps the route it has, A included (`proxy_routes`' `next_route`), so `wait_unlisted` times out, `retire` returns `NotConfirmed` and leaves A running, and the Drain reports `NotRetired` (`UNLISTING_KEEPS` needs the stop between compare and guard).
- B goes Out, or its Machine goes Down, before the guard reads: `last_in` refuses, so A stays In (`LAST_IN_KEEPS` needs the change after the compare). B goes Out only before A is marked leaving; after that, B's own guard would see the mark and refuse.
- The frame frees the Service's Volume, or switches it from V1 to V2 after the compare and before the services role goes off (`SWITCHED_MOVES` needs the switch in that window), and `planDrainVolumes` reads it before admission.

### Outside the property

The carried yes's ask for a Drain shows its effects and no preview (`yesRequired` sends a preview only for `previewed` verbs), so it names no Volume forecast and no move breaks it. The forecast is shown only in the dashboard's Drain dialog (`previewServerDrain`), a dashboard confirm that binds `stops` and nothing of `data`:

| Issue | What runs | F | S |
|---|---|---|---|
| #4556 | The dialog shows a Volume as **Stays**. Its reason clears before `planDrainVolumes` reads, and admission moves it. | 2 | 3 |
| #4556 | The dialog shows a Service as **Stays** on shared Volume V1. A Deploy switches it to V2 on the drained Server before the Engine turns its services role off, and `dataCheck`, reading the newest Container, plans V2's move. | 1 | 3 |

## Keep the model in step

When a change alters a gate on purpose, change the model in the same PR:

1. Update the matching arm of `store_gate`, `send_store`, `send_drain`, `run_drain` or `next_state`, citing the function it copies.
2. A new guard gets a `Broken` flag that removes it and a test that breaks the property without it. A guard with no failing variant is untested.
3. A new place a yes is weighed (a new Store write, a Cloud operation, a Machine route) becomes a `Write`, `Command` or `Route` with its edits, and joins `Command::ALL`.
4. Before handing off, break a guard in the shipped model so it lets a carried yes through (for example make `move_asks`' shortcut `if !facts.protection.asks()`), watch `the_code_binds_every_yes_to_its_plan_and_moment` go red, and put it back. Dropping only `carried &&` makes a shortcut stricter, so it stays green. A `sometimes` that names a window (`LAST_IN_KEEPS`, `UNLISTING_KEEPS`, `SWITCHED_MOVES`) must check the window of its own change (`left`, `stopped`, `data_late`), or closing it stays green: remove the change from that window, including from the attempt before an Inngest retry, and watch it go red.
5. Move the sync commit below forward only after rereading every covered function against the model.

## Bounds and blind spots

Bounds: one request carrying one yes to `Plan::Asked`; one edit of the plan, at any step before the request ends (for a Drain, only before Cloud's last compare); for a Drain, B's Container stops once, B's rotation changes once (Out or Down), B stops answering unseen once and the proxies restart once after A goes Out (each only under `open_paths`), none comes back, B changes only until the proxies drop A, every proxy routes as one, and only one global is retired; one change of the Drain's Volume read; one revocation of the caller's key; one Inngest retry of a Drain's step before the Engine is asked. A plan is one of eight kinds (`Plan`), so digests are equal exactly when plans are, except where `forget_unbound` restores the old collision and for `Started`, which the code's digest doesn't bind. Admission is a free choice: the model holds no Server it reads. The Organization has an approver, so Cloud sends `required` or a yes, never `not_required`.

Out of scope: a Drain's Engine refusing a Server its own read can't see (the model lets the Engine see one Cloud's frame lost, per bet 1); the Drain dialog's forecast (above); a late change that narrows a Drain or that its scope leaves out; what a request carrying no yes may do, including a forgetting shutdown in a Protected Environment, which asks nothing by design; a dashboard confirm; a Deploy that waits at an approval link and the proposal decision behind it; a Project-wide removal; a cancel of a removal Deployment; Cloud's other planned operations (`cleanNamespace`, `setServer`'s ingress-off, unprotecting); a Container creation's signed version, which `AdmittedSpec::still_holds` checks under the exclusive turn; and a Protected Environment toggled mid-request. Cover those with the Store suites in `core/crates/ployz-store/tests/` (`review.rs`, `protected.rs`, `removal.rs`, `deployment.rs`), the dashboard's Postgres tests, and `ployzd`'s `machine_api/authorization_tests.rs`.

## Paths that weigh a yes

The model covers the Store writes, the Drain and the Machine routes above. These other paths take a carried yes and are not modelled. Every Cloud one weighs it in `gateOperation`, the same function the Drain's gate copies, so `gate_skips_yes` already replays 1c93248b9b for all of them. What runs after the gate differs per path, which is why none was added.

| Path | Modelled | Why not, and what covers it |
|---|---|---|
| Server remove (`removeServer`, `operations.server.ts`) | no | Gated on `planRemove` at the request; the run deletes the Data Loss the request confirmed, not what a later read finds. `machine-removal.server.postgres.test.ts`, `machine-removal.inngest.test.ts`. |
| Server forget (`forgetServer`) | no | Remove with no reset and no Data Loss. The same tests; `verify-member-removal` for how Members keep it out. |
| Volume Runs (`runVolume`) | no | Gated once when the run interrupts a Protected Environment; each step after is a Machine's admission. `verify-volume-runs`. |
| Service stop and restart (`actService`) | no | `interrupts` binds the Service, and the Server with `only`, never its copies. `claimServiceAct` reads the copies when its run claims, so a replica that appears after the gate is stopped or restarted too. That is the Service the ask named, so the yes covers it (F2, S1; not filed). `service-act.inngest.postgres.test.ts`. |
| Bulk Server forget (`forgetServers`) | no | The Drain's compare-then-recheck shape: `forgetCluster` refuses when the Servers changed since `check`, inside one Cloud write. `forget-servers.server.postgres.test.ts`. |
| Organization removal (`removeOrganization`) | no | Gated through the CLI's `gated`, then one Cloud teardown. `organization-removal.server.postgres.test.ts`. |
| Cluster rename (`renameAsked`) | no | `alwaysAsks` with no effects; the run writes the name the request asked for. `cluster-name.server.postgres.test.ts`. |
| Ingress Door switch (`switchAsked`) | no | Its run moves the hostnames it reads later, the Drain's late-read shape (#4556). Not checked against this model; the next one to add. `doors.server.postgres.test.ts`. |
| Member role change, member removal, invite revocation | no | `defineCommand` runs `gateOperation`, then `run` in the same request, not in one transaction. `members.functions.postgres.test.ts`, `invitation.server.postgres.test.ts`. |

## Coverage

Reread these when a diff touches them:

- `core/crates/ployz-store/src/trusted.rs`: `Approval`, `Approval::carried`.
- `core/crates/ployz-store/src/review/approval.rs`: `bound`, `Gate::new`, `Gate::retrying`, `asks_cli_member`, `asks_retry`, `ask`, `approve`'s shortcut, `approve_shut_down`, `approve_cancel`, `approve_retry`'s two shortcuts, `move_asks`, `approve_move`.
- `core/crates/ployz-store/src/review/publish.rs`: `publish`'s nothing-staged return.
- `core/crates/ployz-store/src/removal/mod.rs`: `remove`'s call to `gate`, `gate`, `cancel`, `guard`.
- `core/crates/ployz-store/src/removal/due.rs`: `shut_down`, `ask_taking_off`.
- `core/crates/ployz-store/src/deployment/admit.rs`: `admit`'s `Admit::Retry` arm, `retry_review`.
- `core/crates/ployz-store/src/deployment/lifecycle.rs`: `forgettable`, `cancel`.
- `core/crates/ployz-store/src/deployment/history.rs`: `head`'s token.
- `dashboard/src/routes/api/config/-config.handler.ts`: `handleConfigRequest`'s agent refusal and its one Store call.
- `dashboard/src/modules/operations/gate.server.ts`: `gateOperation`, `operationDigest`, `yesRequired`'s `previewed`.
- `dashboard/src/modules/operations/operations.server.ts`: `drainServer`.
- `dashboard/src/modules/machines/server-drain.server.ts`: `stillCovered`, `executeDrainOnce`, `requestServerDrain`, `previewServerDrain`.
- `dashboard/src/modules/machines/server-operations.server.ts`: `drainPlan`.
- `dashboard/src/modules/machines/server-drain-volumes.server.ts`: `planDrainVolumes`, `admitDrainVolume`, `moveRefusal`.
- `dashboard/src/modules/machines/server-drain-volumes.ts`: `dataCheck`.
- `core/crates/ployz/src/drain.rs`: `preflight` and `chosen_on`, which refuse any Service with a Global copy, and the leftover `copies_on`, which never selects one.
- `core/crates/ployz/src/deploy/cleanup.rs`: `retire`, `take_out`, `send_out`, `wait_unlisted`, `retire_unconfirmed`.
- `core/crates/ployz-core/src/service/serving.rs`: `proxy_routes`, `next_route`, `ServingContainer::prove`.
- `core/crates/ployzd/src/reach.rs`: `Reach::new`, `pass`, `keep` (a restart's empty `current`), and `core/crates/ployzd/src/ingress.rs`: `next_pass`, `route_acks`.
- `core/crates/ployz-core/src/service.rs`: `observed_global_slot`, and `core/crates/ployzd/src/machine/local_machine/admission.rs`: the `Scope::Full` arm (how a user's Global reaches a Drain).
- `core/crates/ployz-core/src/domain/observation.rs`: `traffic_address`, `may_serve`.
- `core/crates/ployzd/src/machine/local_machine/rotation.rs`: `last_in`, `in_siblings`.
- `dashboard/src/modules/service-act/service-act.server.ts`: `claimServiceAct` (the table row above).
- `core/crates/ployzd/src/machine/local_machine/turns.rs`: `admit_installation`, `finish_mutation`, `finish_shared`, `finish_admitted`, `finish_installed`, `exclusive_turn`.
- `core/crates/ployzd/src/machine/local_machine.rs`: `require_management_access`.
- `core/crates/ployzd/src/machine/local_machine/container.rs`: every `exclusive_turn()` call inside `finish_installed` work.
- `core/crates/ployzd/src/machine/local_machine/management_client.rs`: `set_device`.

Sync commit: `db6e1366e4170507b4a9da336b1b92399a28a520`, the `main` each function above was reread against. Code in the list that changed after it has not been rechecked.
