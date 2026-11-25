---
name: verify-approval-binding
description: Model-check that a person's yes (a carried approval digest, a Drain's yes, a Device's access on a Machine) is weighed against the plan and the keys as they stand when its effect runs, with the Stateright model in core/crates/ployz-verify/src/approval_binding.rs. Use when a change touches a carried yes or a shortcut past one: Approval::carried in ployz-store's trusted.rs; Gate, approve, approve_retry, approve_shut_down, approve_cancel, move_asks or approve_move in ployz-store's review/approval.rs; publish's early return in review/publish.rs; gate, guard, remove or cancel in removal/mod.rs; shut_down or ask_taking_off in removal/due.rs; the Admit::Retry arm in deployment/admit.rs; forgettable or cancel in deployment/lifecycle.rs; head's token in deployment/history.rs; handleConfigRequest in dashboard/src/routes/api/config/-config.handler.ts; gateOperation in dashboard/src/modules/operations/gate.server.ts; drainServer in operations.server.ts, stillCovered and executeDrainOnce in machines/server-drain.server.ts, drainPlan or drainRefusal in machines/server-operations.server.ts, planDrainVolumes or admitDrainVolume in machines/server-drain-volumes.server.ts, dataCheck in machines/server-drain-volumes.ts, holderOn in volume-run/volume-run.cluster.server.ts, or holderSwapped in volume-run/volume-run.inngest.ts; the Engine drain's preflight, services_off or chosen_on in ployz/src/drain.rs; or ployzd's admission turns (turns.rs finish_*, exclusive_turn, admit_installation, require_management_access) and the work that takes exclusive_turn itself in machine/local_machine/container.rs.
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

- **The code holds.** `the_code_binds_every_yes_to_its_plan_and_moment` checks every command as it ships (Save, Deploy, Retry, RemovalRetry, ShutDown, Remove, Cancel, Move, Drain, a turned Machine route), prints each state count, and fails on any unreached `sometimes`: a branch the model can no longer reach makes a passing property meaningless. It leaves out the open paths below: the installed route, and the change `open_paths` adds (a Cancel's Deployment starting).
- **The past bugs and guards.** Each test sets one `Broken` flag that restores the code before a fix, or removes a guard, and expects a counterexample. If one goes green, the guard stopped mattering or the model lost the path that shows it.
- **The open paths.** Two tests pass because the shipped code breaks the property (below). When one is fixed, its test goes red: move its change into the shipped model and give the fix a `Broken` flag and a breaking test, as the others have.

| Fix | `Broken` flag | Command |
|---|---|---|
| f06d2a6351 | `cloud_calls_twice`, `approve_skips_yes` | Save, Deploy |
| 0f29e0d3f3 | `empty_save_skips_yes`, `forget_unbound`, `cancel_skips_unless_theirs` | Save, ShutDown, RemovalRetry, Cancel |
| 8836d08f2f | `retry_skips_yes`, `protector_retry_skips_yes`, `forget_skips_yes`, `shutdown_skips_yes`, `remove_skips_yes`, `cancel_skips_unprotected`, `move_skips_yes` | Retry, ShutDown, RemovalRetry, Remove, Cancel, Move |
| 1c93248b9b | `agent_yes_unrefused`, `gate_skips_yes`, `drain_runs_harmless` | Save, Drain |
| bad55e1aed | `drain_runs_gone` | Drain |
| 273b77fabc | `turn_trusts_arrival` | Machine, turned route |
| #4567 | `engine_skips_globals` (drains beside a user Global, as `main` did) | Drain |
| #4567, round 4 | `engine_global_scoped`, `cloud_reads_unanswered`, `volume_moves_global` | Drain |
| guard | `member_yes_answers_protected` | Save |
| guard | `engine_reads_unlisted_empty` | Drain |
| guard | `move_skips_holder` (the holder check at admission and each Move step) | Drain |

273b77fabc's teardown statements were removed in cfd221baf1, so its flag replays the shape (checked at arrival, run under a later turn) on what authorizes a call today, the caller's management access. Its other half, enforcement turned on while a call waited, cannot be expressed: every daemon has enforced since a03ef88eea.

## Read a counterexample

Each failing path prints as a numbered story, one step per row in the code's words, then the state:

```
 1. editor: the plan changes to one that destroys nothing
    plan=Harmless key=accepted exec=Ready wrote=nothing
 2. executor: handleConfigRequest sends the yes into the write, approve_retry weighs it there (under Broken), and it runs
    plan=Harmless key=accepted exec=Ran wrote=Harmless under a yes that counts
```

`plan` is the plan as it stands, and `Asked` is the one the person said yes to. `key` is whether the Machine accepts the caller. A Drain's rows add `server`, the drained Server: the user Global that landed there, if any (`chosen` when it is a Service the Drain's scope chose, and the Drain step it landed before: `Gate`, `Compare`, `Read` for the Engine's first read, `Reread` for its second); what Cloud reads of it (`it now`, or `its last rows` once it stopped answering and the frame lists it `unanswered`); whether the Engine can list it; and whether its services are on. `data` is what `dataCheck` reads of the Service left for its Volume (`Global` once a Full caller replaced it with a Global after the Engine's report), `swapped` is the step before which a Full caller replaced the planned holder with a Global (`Admit` or `Cut`), and `admission` is what admission and the Move did with a planned move (`HolderRefused` when the holder check refused it). `wrote` is the effect and whether its yes counted when it ran. A Drain that runs beside a user Global writes `Global`. A break is any `wrote` other than `Asked under a yes that counts`. `assert_properties` prints raw actions instead; to read one as a story, call `breaks_ending` with that model in a scratch test.

## Real bug or model error

A counterexample is a lead. Check each row against the function it names (the `Write`, `Route` and `Action` doc comments give file and function) before calling it a bug.

- If a step does what the code cannot, fix the model and say so. The usual suspects are `store_gate`'s per-write shortcuts, `protection_asks` (which writes a Protected Environment's protection asks about), `ApprovalBinding::edits` (which plans the editor can reach, and when), `server_changes` and `data_changes` (when the drained Server and the Volume read change), `Plan::digest` (which changes the code's digest binds), and the Store write being one atomic step: it runs under the Environment's lock, which the editor's write takes too. A Drain is not one step: Cloud compares, the Engine lists the Server, turns its services off and lists it again, Cloud plans the Volume moves from a new frame, admission admits or refuses each, and each Move's steps check its holder again (`holderOn`, and the Drain's pin in `holderSwapped`). A retry runs the compare again, so a change made during the dead attempt counts as before the retry's compare.
- If every step matches, it is a finding. Report it with the story, scored per AGENTS.md. Keep the property: weakening it to go green hides the bug.

## The open paths

Each is scored per AGENTS.md, filed when a user can reach it, and left in the code. Its test passes while the code breaks the property, except the last row, which the model does not cover.

| Issue | Test | What runs | F | S |
|---|---|---|---|---|
| #4553 | `an_installed_step_runs_for_a_key_revoked_while_it_drained` | `finish_installed` checks the caller's management access when the call arrives; its work drains, takes `exclusive_turn`, which checks nothing, and stops or removes a Container for a Device `set_device` removed meanwhile. Removing a Container can delete its only anonymous-volume data. Fixed by #4558 (open), which checks under each turn the work takes. | 1 | 10 |
| #4555 | `a_cancel_s_yes_while_queued_stops_a_started_deployment` | A carried-digest case. `approve_cancel` binds the Deployment and the review version; Head's token counts only ended Deployments, so the runner's claim moves neither, and a carried yes to take a queued Deployment out of the queue stops it partway once it runs (`Plan::Started`). No end-to-end approval path is shown: an authorized caller's cancel skips the first ask, and a protected member's gets a refusal. | 1 | 4 |
| none | not modelled | A stop-first rollout of a Service with 2 replicas, live today. `last_in` lets the healthy copy go Out while the other crash-loops, counting it In from its rotation alone, so the Service has no serving copy until the rollout's new copy is healthy. The gap heals within the deploy, and no yes is weighed in it, so it sits outside this property. F×S = 6, under the fix bar. | 2 | 3 |

A Drain retires no Global. Up to #4567 it retired a user Global's copy on the drained Server past `last_in`, which counts a crash-looping copy elsewhere In, and past `wait_unlisted`, which a restarted proxy passes, so it could remove the last copy that served (four rows here, #4556 among them). A Full caller can still make a user Global: the Machine API admits a resolved Global spec (`ployzd` `admission.rs`, `Scope::Full`), and Compose accepted `deploy.mode: global` up to v0.1.2-beta.41. So the Drain refuses any user Global on its Server, chosen by its scope or not, and touches no system Global (`DrainScope::includes` drops `ployz-system`). `last_in` still counts a copy In from its rotation alone, which the rollout row meets outside a Drain.

`open(command)` in the tests turns on `open_paths`. `the_code_binds_every_yes_to_its_plan_and_moment` runs without it. There, a Drain meets every change to the drained Server and the Volume read that the code reads:
- A user Global lands before Cloud's gate or its last compare: `drainRefusal` refuses it (`CLOUD_REFUSES_GLOBAL`). One the scope left out lands after the compare: `chosen_on` refuses it at either Engine read (`ENGINE_REFUSES_UNCHOSEN` needs it outside the scope and after the compare). One that lands between the two reads refuses with services off (`OFF_REFUSED`).
- The Server stops answering Cloud before its last compare: the frame lists it `unanswered`, and `drainRefusal` refuses it though its last rows match the yes (`UNANSWERED_REFUSES` needs those rows to be the asked plan while the plan has moved on).
- The Server stops answering the Engine's listing: `chosen_on` refuses it, a Global there unseen (`UNLISTED_REFUSES`).
- A Full caller replaces the Service left for its Volume with a Global after the Engine's report: `dataCheck` keeps the Volume (`GLOBAL_KEEPS_VOLUME`).
- A Full caller replaces the Volume's holder with a Global after `planDrainVolumes` planned its move: admission's holder check refuses one that lands before it, and the Move step's one that lands after (`HOLDER_REFUSES` needs the swap after admission).
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
4. Before handing off, break a guard in the shipped model so it lets a carried yes through (for example make `move_asks`' shortcut `if !facts.protection.asks()`), watch `the_code_binds_every_yes_to_its_plan_and_moment` go red, and put it back. Dropping only `carried &&` makes a shortcut stricter, so it stays green. A `sometimes` that names a window (`ENGINE_REFUSES_UNCHOSEN`, `OFF_REFUSED`, `UNANSWERED_REFUSES`, `SWITCHED_MOVES`, `HOLDER_REFUSES`) must check the window of its own change (`Landed::before`, `off`, `silent`, `data_late`, `swapped`), or closing it stays green: remove the change from that window, including from the attempt before an Inngest retry, and watch it go red.
5. Move the sync commit below forward only after rereading every covered function against the model.

## Bounds and blind spots

Bounds: one request carrying one yes to `Plan::Asked`; one edit of the plan, at any step before the request ends (for a Drain, only before Cloud's last compare); for a Drain, one user Global lands on the drained Server until the Engine's second read, the Server stops answering Cloud once until Cloud's last compare and stops answering the Engine once until that second read, and none comes back; one change of the Drain's Volume read, and one swap of its holder for a Global after the move is planned; one revocation of the caller's key; one Inngest retry of a Drain's step before the Engine is asked. A plan is one of eight kinds (`Plan`), so digests are equal exactly when plans are, except where `forget_unbound` restores the old collision and for `Started`, which the code's digest doesn't bind. Admission is a free choice: the model holds no Server it reads. The Organization has an approver, so Cloud sends `required` or a yes, never `not_required`.

Out of scope: the Drain dialog's forecast (above); a late change that narrows a Drain; what a request carrying no yes may do, including a forgetting shutdown in a Protected Environment, which asks nothing by design; a dashboard confirm; a Deploy that waits at an approval link and the proposal decision behind it; a Project-wide removal; a cancel of a removal Deployment; Cloud's other planned operations (`cleanNamespace`, `setServer`'s ingress-off, unprotecting); a Container creation's signed version, which `AdmittedSpec::still_holds` checks under the exclusive turn; and a Protected Environment toggled mid-request. Cover those with the Store suites in `core/crates/ployz-store/tests/` (`review.rs`, `protected.rs`, `removal.rs`, `deployment.rs`), the dashboard's Postgres tests, and `ployzd`'s `machine_api/authorization_tests.rs`.

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
- `dashboard/src/modules/machines/server-operations.server.ts`: `drainPlan`, `drainRefusal`, `unseenDrain`.
- `dashboard/src/modules/machines/server-drain-volumes.server.ts`: `planDrainVolumes`, `admitDrainVolume`, `moveRefusal`.
- `dashboard/src/modules/machines/server-drain-volumes.ts`: `dataCheck`.
- `dashboard/src/modules/volume-run/volume-run.cluster.server.ts`: `holderOn`, which `readHolder`, `findHolder` and `startRefusal` share; `dashboard/src/modules/volume-run/volume-run.inngest.ts`: `holderSwapped`.
- `core/crates/ployz/src/drain.rs`: `preflight`'s first listing, `services_off`'s second, and `chosen_on`, which refuses an unlisted Server and any user Global on it, chosen or not.
- `core/crates/ployz-core/src/service.rs`: `observed_global_slot`, and `core/crates/ployzd/src/machine/local_machine/admission.rs`: the `Scope::Full` arm (how a user's Global reaches a Drain).
- `dashboard/src/modules/service-act/service-act.server.ts`: `claimServiceAct` (the table row above).
- `core/crates/ployzd/src/machine/local_machine/turns.rs`: `admit_installation`, `finish_mutation`, `finish_shared`, `finish_admitted`, `finish_installed`, `exclusive_turn`.
- `core/crates/ployzd/src/machine/local_machine.rs`: `require_management_access`.
- `core/crates/ployzd/src/machine/local_machine/container.rs`: every `exclusive_turn()` call inside `finish_installed` work.
- `core/crates/ployzd/src/machine/local_machine/management_client.rs`: `set_device`.

Sync commit: `db6e1366e4170507b4a9da336b1b92399a28a520`, the `main` each function above was reread against. Code in the list that changed after it has not been rechecked.
