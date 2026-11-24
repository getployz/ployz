---
name: verify-approval-binding
description: Model-check that a person's yes (a carried approval digest, a Drain's yes, a Device's access on a Machine) is weighed against the plan and the keys as they stand when its effect runs, with the Stateright model in core/crates/ployz-verify/src/approval_binding.rs. Use when a change touches a carried yes or a shortcut past one: Approval::carried in ployz-store's trusted.rs; Gate, approve, approve_retry, approve_shut_down, approve_cancel, move_asks or approve_move in ployz-store's review/approval.rs; publish's early return in review/publish.rs; gate, guard, remove or cancel in removal/mod.rs; shut_down or ask_taking_off in removal/due.rs; the Admit::Retry arm in deployment/admit.rs; forgettable in deployment/lifecycle.rs; handleConfigRequest in dashboard/src/routes/api/config/-config.handler.ts; gateOperation in dashboard/src/modules/operations/gate.server.ts; drainServer in operations.server.ts or stillCovered and executeDrainOnce in machines/server-drain.server.ts; or ployzd's admission turns (turns.rs finish_*, exclusive_turn, admit_installation, require_management_access) and the work that takes exclusive_turn itself in machine/local_machine/container.rs.
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

Green is 17 passing tests, in three groups:

- **The code holds.** `the_code_binds_every_yes_to_its_plan_and_moment` checks every command as it ships (Save, Deploy, Retry, RemovalRetry, ShutDown, Remove, Cancel, Move, Drain, a turned Machine route), prints each state count, and fails on any unreached `sometimes`: a branch the model can no longer reach makes a passing property meaningless.
- **The past bugs.** Each test sets one `Broken` flag that restores the code before a fix and expects a counterexample. If one goes green, the guard stopped mattering or the model lost the path that shows it.
- **The finding.** `an_installed_step_runs_for_a_key_revoked_while_it_drained` passes because the shipped code breaks the property (below). When the code is fixed, flip it into a `Broken` flag and a breaking test, as the others are.

| Commit | `Broken` flag | Command |
|---|---|---|
| f06d2a6351 | `cloud_calls_twice`, `approve_skips_yes` | Save, Deploy |
| 0f29e0d3f3 | `empty_save_skips_yes`, `forget_unbound`, `cancel_skips_unless_theirs` | Save, ShutDown, RemovalRetry, Cancel |
| 8836d08f2f | `retry_skips_yes`, `protector_retry_skips_yes`, `forget_skips_yes`, `shutdown_skips_yes`, `remove_skips_yes`, `cancel_skips_unprotected`, `move_skips_yes` | Retry, ShutDown, RemovalRetry, Remove, Cancel, Move |
| 1c93248b9b | `agent_yes_unrefused`, `gate_skips_yes`, `drain_runs_harmless` | Save, Drain |
| bad55e1aed | `drain_runs_gone` | Drain |
| 273b77fabc | `turn_trusts_arrival` | Machine, turned route |
| guard | `member_yes_answers_protected` | Save |

273b77fabc's teardown statements were removed in cfd221baf1, so its flag replays the shape (checked at arrival, run under a later turn) on what authorizes a call today, the caller's management access. Its other half, enforcement turned on while a call waited, cannot be expressed: every daemon has enforced since a03ef88eea.

## Read a counterexample

Each failing path prints as a numbered story, one step per row in the code's words, then the state:

```
 1. editor: the plan changes to one that destroys nothing
    plan=Harmless key=accepted exec=Ready wrote=nothing
 2. executor: handleConfigRequest sends the yes into the write, approve_retry weighs it there (under Broken), and it runs
    plan=Harmless key=accepted exec=Ran wrote=Harmless under a yes that counts
```

`plan` is the plan as it stands, and `Asked` is the one the person said yes to. `key` is whether the Machine accepts the caller. `wrote` is the effect and whether its yes counted when it ran. A break is any `wrote` other than `Asked under a yes that counts`. `assert_properties` prints raw actions instead; to read one as a story, call `breaks_ending` with that model in a scratch test.

## Real bug or model error

A counterexample is a lead. Check each row against the function it names (the `Write`, `Route` and `Action` doc comments give file and function) before calling it a bug.

- If a step does what the code cannot, fix the model and say so. The usual suspects are `store_gate`'s per-write shortcuts, `protection_asks` (which writes a Protected Environment's protection asks about), `Command::edits` (which plans the editor can reach), and the Store write being one atomic step: it runs under the Environment's lock, which the editor's write takes too.
- If every step matches, it is a finding. Report it with the story, scored per AGENTS.md. Keep the property: weakening it to go green hides the bug.

## The open finding

Work admitted through `finish_installed` (`ployzd/src/machine/local_machine/turns.rs`) checks the caller's management access once, in `admit_installation`, when the call arrives. The work then drains and takes `exclusive_turn` itself, which checks nothing, and acts: `ContainerStep::StopThenRemove` and `ContainerStep::Remove` in `container.rs`, `stop_checked`'s stop, and a Volume cut. A Device removed by `set_device` while its own stop or removal drained still gets its Container stopped or removed. Nothing leaks: the Device had access when it asked. Scored F=1, S=3. `finish_mutation` and `finish_shared` take the turn first and check under it, which is the shape the fix would copy.

## Keep the model in step

When a change alters a gate on purpose, change the model in the same PR:

1. Update the matching arm of `store_gate`, `send_store`, `send_drain`, `run_drain` or `next_state`, citing the function it copies.
2. A new guard gets a `Broken` flag that removes it and a test that breaks the property without it. A guard with no failing variant is untested.
3. A new place a yes is weighed (a new Store write, a Cloud operation, a Machine route) becomes a `Write`, `Command` or `Route` with its edits, and joins `Command::ALL`.
4. Before handing off, break a guard in the shipped model (for example drop `carried &&` from one shortcut), watch `the_code_binds_every_yes_to_its_plan_and_moment` go red, and put it back.
5. Move the sync commit below forward only after rereading every covered function against the model.

## Bounds and blind spots

Bounds: one request carrying one yes to `Plan::Asked`; one edit of the plan, at any step before the request ends; one revocation of the caller's key; one Inngest retry of a Drain's step. A plan is one of six kinds (`Plan`), so digests are equal exactly when plans are, except where `forget_unbound` restores the old collision. The Organization has an approver, so Cloud sends `required` or a yes, never `not_required`.

Out of scope: what a request carrying no yes may do, including a forgetting shutdown in a Protected Environment, which asks nothing by design; a dashboard confirm; a Deploy that waits at an approval link and the proposal decision behind it; a Project-wide removal; a cancel of a removal Deployment; Cloud's other planned operations (`cleanNamespace`, `setServer`'s ingress-off, unprotecting); a Container creation's signed version, which `AdmittedSpec::still_holds` checks under the exclusive turn; and a Protected Environment toggled mid-request. Cover those with the Store suites in `core/crates/ployz-store/tests/` (`review.rs`, `protected.rs`, `removal.rs`, `deployment.rs`), the dashboard's Postgres tests, and `ployzd`'s `machine_api/authorization_tests.rs`.

## Coverage

Reread these when a diff touches them:

- `core/crates/ployz-store/src/trusted.rs`: `Approval`, `Approval::carried`.
- `core/crates/ployz-store/src/review/approval.rs`: `bound`, `Gate::new`, `Gate::retrying`, `asks_cli_member`, `asks_retry`, `ask`, `approve`'s shortcut, `approve_shut_down`, `approve_cancel`, `approve_retry`'s two shortcuts, `move_asks`, `approve_move`.
- `core/crates/ployz-store/src/review/publish.rs`: `publish`'s nothing-staged return.
- `core/crates/ployz-store/src/removal/mod.rs`: `remove`'s call to `gate`, `gate`, `cancel`, `guard`.
- `core/crates/ployz-store/src/removal/due.rs`: `shut_down`, `ask_taking_off`.
- `core/crates/ployz-store/src/deployment/admit.rs`: `admit`'s `Admit::Retry` arm, `retry_review`.
- `core/crates/ployz-store/src/deployment/lifecycle.rs`: `forgettable`.
- `dashboard/src/routes/api/config/-config.handler.ts`: `handleConfigRequest`'s agent refusal and its one Store call.
- `dashboard/src/modules/operations/gate.server.ts`: `gateOperation`.
- `dashboard/src/modules/operations/operations.server.ts`: `drainServer`.
- `dashboard/src/modules/machines/server-drain.server.ts`: `stillCovered`, `executeDrainOnce`.
- `core/crates/ployzd/src/machine/local_machine/turns.rs`: `admit_installation`, `finish_mutation`, `finish_shared`, `finish_admitted`, `finish_installed`, `exclusive_turn`.
- `core/crates/ployzd/src/machine/local_machine.rs`: `require_management_access`.
- `core/crates/ployzd/src/machine/local_machine/container.rs`: every `exclusive_turn()` call inside `finish_installed` work.
- `core/crates/ployzd/src/machine/local_machine/management_client.rs`: `set_device`.

Sync commit: `db6e1366e4170507b4a9da336b1b92399a28a520`, the `main` each function above was reread against. Code in the list that changed after it has not been rechecked.
