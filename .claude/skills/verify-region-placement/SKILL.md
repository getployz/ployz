---
name: verify-region-placement
description: Model-check what a runner's Deploy does to a Service's replicas as Servers change region, join, leave and stop taking new starts, and as the Environment's region, a Service's counts and its version change, with the Stateright model in core/crates/ployz-verify/src/region_placement.rs, which calls the real planner. Use when a change touches region placement in the Deploy planner (`plan_one_service`, `spread_machines`, `eligible_machines`, or `placement_error` in core/crates/ployz/src/deploy/planning.rs; `Spread`, `Quota`, `select_replicated`, `plan_replicated` or `withdraw` in planning/placement.rs; `refuse_kept_apart`, `reserve_shared`, `place`, `constrain_candidates` or the Volume-outside-region checks in planning/volumes.rs; `PlanError::blocked_kind` in deploy.rs), the lowering of `replicas` and the Environment's region (ployz-core config/lowering.rs), or the Store's record of a Deploy that waits (`node_outcomes` in ployz-store deployment/outcomes.rs, `all_waiting` in deployment/mod.rs, `live` in deployment/history.rs, the `VolumeInSeveralRegions` rule in rules.rs).
---

# Verify region placement

The model in `core/crates/ployz-verify/src/region_placement.rs` explores every order of Deploys and changes to a small Cluster, up to a bound. Each Deploy calls the real planner, `ployz::deploy::preview_deploy`, with a runner's options (`block_unmountable: true`), on a `DeploySnapshot` built from the model's world. The world then takes the operations the preview returns, each as a step that completes. So a planner change is checked as written, with no copy to keep in step.

Three pieces are hand copies, and drift unless you keep them in step:

- `RegionPlacement::requested` lowers a Service's `replicas` and the Environment's region into its spec, as `lower_snapshot` in ployz-core `config/lowering.rs` does: counts by region set `regions` and no constraint, and one count takes the Environment's region as `node.labels.region==X`.
- `counts_menu` never offers counts in two regions to a Service with a Volume, because the Store refuses that config at save (`VolumeInSeveralRegions` in ployz-store `rules.rs`).
- `Verdict::advanced`, `applied`, `all_waiting` and `live` copy the Store's record of the Deploy. A Service's node is Not attempted when blocked, Deployed when the preview planned work for it, and Unchanged otherwise (`node_outcomes`). A Refusal fails the Deployment. `all_waiting` (deployment/mod.rs) reads it as nothing applied, and `live` (deployment/history.rs) skips such a Deployment.

## Run it

```
cd core && CARGO_INCREMENTAL=0 cargo test -p ployz-verify region_placement
```

About 80 s warm on four threads, plus a few minutes to build `ployz` cold. CI runs it in the workspace nextest, so a red model blocks the PR.

It is green when all nine tests pass:

- `plain_holds_but_for_its_findings`, `volume_holds_but_for_its_findings` and `shared_holds_but_for_its_findings` check every state of a scenario. Each property holds, except the ones its test names as findings, which must fail. Each outcome in `REACHED` must be reached, except the ones the test names as unreached in that scenario. Each prints its state count: about 122,000, 40,600 and 11,700.
- One test per finding replays a fixed schedule with `follow` and asserts that its last Deploy breaks the property. It prints the schedule as a story.
- `nothing_applied_needs_the_partial_wait` sets `Broken::all_waiting_needs_unattempted`, the Store's `all_waiting` before R5's fix in 2e3364db24, and expects "a Deploy that changed nothing on the Servers and left a Wait reads nothing applied and does not move Live" to fail.

A finding that gets fixed turns its scenario's `holds` test red, because the property now holds. Take it out of that test's findings, and turn its schedule test into one that asserts the property holds on that path.

## The world

Three Servers: `one` (fsn), `two` (syd) and `three` (no region), all taking new starts. Two regions, fsn and syd, plus no region. A Server's region is its `region` label, and taking new starts is `accepts_services`. A replica already running on a Server that takes no new starts keeps running.

Three scenarios, each started by one Deploy of its start config:

- `Plain`: `api` alone, counted `{"fsn":1,"syd":1}`, with no Environment region.
- `Volume`: `api` mounts the provisioned Volume `data`, whose writer copy is on `one`. One count, 1, in the Environment's region fsn.
- `Shared`: `api` and `worker` both mount `data` on `one`. One count of 1 each, in fsn.

Between Deploys, any of these changes, each costing one from the path's budget (4 in Plain and Volume, 3 in Shared):

- `Relabel(server, label)`, `Add(server)` (joins with no region, taking new starts), `Remove(server)` (its Containers go with it; never `one` when it holds `data`), and `ToggleAccepts(server)`.
- `Counts(service, replicas)` from `counts_menu`: 1 or 2, or counts by region up to `{"fsn":2,"syd":1}` and `{"fsn":1,"syd":2}`, in one region only for a Service with a Volume.
- `Region(label)`, the Environment's region, and `Bump(service)`, a new image version (once).

`Deploy` is offered in every state and costs nothing.

## Properties

Each property is a sentence about one Deploy, judged from the world before and after it (`Verdict::judge`).

- "with no config change and no Server gone, a Deploy removes a running replica only past the counts, from a Server that takes no new starts, or as a spare whose waiting region starts its own". Harm: a healthy replica goes though the user changed nothing. The allowed removals are the documented ones. A replica past the counts goes. One on a Server that takes no services moves at the next Deploy (servers/manage-servers.md). A replica moves when another starts in its region (regions.md, "spreads ... as usual"). A spare, outside the counts or past its own region's count, goes once a region short of its count starts one (regions.md).
- "after a Deploy, a Service it does not refuse runs no more replicas than its counts add up to". Harm: a smaller count isn't carried out.
- "while another region waits, a region whose count is met on Servers that take new starts starts nothing new when only labels or new Servers changed". Harm: a relabel starts replicas nobody asked for. The scope is R5's own: "start nothing new in a region whose count is met while another waits". With nothing waiting, a new Server in a region takes a replica as main's spread always did.
- "a Wait names only regions still short of their count". Harm: the user is told to fix a region that is fine.
- "a Deploy that changed nothing on the Servers and left a Wait reads nothing applied and does not move Live". Harm: Live names a config the Servers don't run.
- "a Service waits only while a Server or label it needs is missing". Harm: a Wait that no Server or label can clear (`Verdict::can_run`). Each region the config keeps it in needs a present Server there that takes new starts. A Service with the Volume also needs `one` to take new starts and be in its one configured region.
- "a Refusal clears only with a config change". Harm: a Refusal tells the user to change the config when relabelling would do, and the opposite.
- "a Service is refused only when it shares the Volume with one whose configured regions can never meet its own" (`Config::meet`). Harm: a Service that could run is refused.
- "a Service with a Volume runs only on the Volume writer's Server".
- "a Deploy never leaves a Service fewer running replicas, unless a count went down or a Server went away".
- "a Deploy that leaves replicas on the old version says how many". Harm: replicas run an old release while the Deploy reads as done. Not checked on a refused Service.
- "a Deploy repeated with nothing changed changes nothing on the Servers". Harm: a Deploy is not a fixed point.

The `sometimes` rows in `REACHED` show the model reaches what it claims to check: a partial wait, a whole wait, a Refusal, a spare kept for a waiting region, a Deploy held back from Live, a whole Deploy the planner refuses, and a Wait that counts a replica on the old version.

## Findings

The shipped planner breaks five properties. Each has a schedule test, and its scenario's `holds` test lists the property.

1. **A relabel removes a spare its region still waits for** (#4368, `a_relabel_removes_a_spare_its_region_still_waits_for`). `one` (fsn) stops taking new starts. `two` loses its syd label, so syd waits and `two`'s replica stands in for it. `three` is labelled fsn. The Deploy starts api on `three` and removes `two`'s replica, while syd still waits and `one`'s replica, on a Server that takes no services, stays. The planner's holder-first pass sees only Servers that take new starts, and `Spread::hold` runs after the new start is chosen.
2. **A whole Wait counts no replica on the old version** (#4369, `a_whole_wait_counts_no_replica_on_the_old_version`). `one` and `two` lose their labels and api moves to v2. The Deploy reads "api waits: no Server in fsn or syd" with no count of the two v1 replicas still running. `plan_one_service` returns `RegionUnmatched` in place of the counts `Spread::hold` computed when no region starts and nothing is planned. The model finds the same gap wherever a Service waits whole: one count in an Environment region with no Server, or a Volume outside the region.
3. **A whole Wait keeps the surplus of a smaller count** (new, `a_whole_wait_keeps_the_surplus_of_a_smaller_count`). api runs 2 replicas. Its count goes to 1, the Environment's region to fsn, and `one` loses its label. The Deploy reads "api waits: no Server in fsn" and both replicas keep running. With one count, `spread_machines` fails on `eligible_machines` before `withdraw` can run. The Volume scenarios reach it too, through a Volume outside the Service's region. This is the one-count form of the R5 ruling that the surplus is removed in every case.
4. **A kept spare is updated by the next Deploy** (new, `a_kept_spare_is_updated_by_the_next_deploy`). api counted `{"fsn":1,"syd":2}` runs one fsn and two syd replicas. `one` is relabelled syd and the counts go to `{"fsn":1,"syd":1}`. The Deploy removes the syd surplus and keeps `one`'s replica as fsn's spare, without its new placement. The next Deploy, with nothing changed, updates that spare in place. `Spread::hold` keeps a spare as it is, and the next Deploy picks it as a syd replica.
5. **Relabelling the Volume's Server moves the Refusal** (new, `relabelling_the_volume_server_moves_the_refusal`). api counted in syd and worker in fsn share `data` on `one` (fsn), so api is refused. Relabelling `one` syd, with no config change, deploys api and refuses worker instead. The two configs can never meet, so one of them is always refused, but which one follows the Volume Server's label. That contradicts `BlockedKind::Refusal`'s own doc ("only a config change deploys it").

A sixth gap is main's behavior, outside the model's properties. A Service with one count and no region, whose every Server takes no services, fails the whole Deploy (`NoEligibleMachines` is not among the errors `assemble_plan` blocks). The `PLAN_FAILS` row reaches it.

## Read a counterexample

A `holds` test prints each failing property with the actions of a path to it. That path is the checker's own discovery, which is not the shortest. To read it, replay the actions in a scratch test with `follow`. It prints the start world, then each step with what the Deploy planned or left waiting, the world after it, and the properties it broke:

```rust
#[test]
fn scratch() {
    crate::harness::follow(
        RegionPlacement::new(Scenario::Plain, 4),
        &[Action::ToggleAccepts(0), Action::Relabel(1, Label::None), Action::Relabel(2, Label::Fsn), Action::Deploy],
    );
}
```

For the shortest path in BFS order, use `crate::harness::first_break(model, PROPERTY, |_| true, true)`. It runs on one thread, about a minute on Plain. Delete the scratch test after.

These Stateright paths can't go through `/statechart`, which runs TLA+ specs only. The `story` is their plain-words form.

## Real bug or model error

A counterexample is a lead. Check each step of the story against the planner and the docs before calling it a bug. These model errors were found and fixed while the model was built:

- A replica past its own region's count after a relabel is a spare by regions.md's definition. The first version counted only replicas outside the counted regions, and flagged a documented removal.
- A Service with one count and no region has every Server in its region, so moving one of its replicas to a new Server is a move. The first version compared labels, and flagged main's spread.
- A replica on a Server that takes no services doesn't meet its region's count, because the next Deploy moves it (manage-servers.md). The first version counted it as met.
- A Container's spec records its Volumes scoped to the Namespace, and the planner compares against a scoped spec (`scope_requested`). The first version compared unscoped specs, and read every Volume replica as outdated.

When each step matches the code and the docs, it is a finding. Report it in the code's words with the schedule. Keep the property, and add the schedule as a test.

## A break that stays green

The planner pins a Volume's Service to the writer in two places: `constrain_candidates` in `plan_one_service`, and `VolumePlan::place`. Removing one leaves the other, and "a Service with a Volume runs only on the Volume writer's Server" holds. Break both to see it fail.

## Proof that the properties can fail

Each property that holds on the shipped code was seen failing on a scratch change to the real planner, which was then reverted:

| Scratch change | Fails |
| --- | --- |
| `Spread::hold` keeps no Container in a waiting region's place (drop `kept.extend(in_its_place ...)`) | "never fewer running replicas" (Plain), the label-change removal property (Volume) |
| `Spread::hold` names every waiting region (`if settled < share.count` to `if true`) | "a Wait names only regions still short" |
| `select_replicated` loses its holder-first round (`rounds` always `&[false]`) | the met-region property |
| `PlanError::blocked_kind` returns Refusal for every error | "refused only when ... can never meet" |
| `Spread::wait_for` puts every counted region in `waiting` | "waits only while a Server or label it needs is missing" |
| Both Volume pins removed (above) | "runs only on the Volume writer's Server" |
| `Broken::all_waiting_needs_unattempted` in the model | "reads nothing applied and does not move Live" |

The other five fail on the shipped code.

## Keep the model in step

When a change alters region placement on purpose:

1. Run the model. A planner change needs nothing else unless a property's meaning changed.
2. If the change alters lowering or the Store's record of a Deploy, change the copy in the same PR, citing the function.
3. If it fixes a finding, update the `holds` test and the finding's schedule test as above, and say so in the PR body.
4. Narrow a property only when the code is right and the property was wrong about the documented design. Cite the doc or ruling in the property's comment.
5. Move the sync commit below forward only after you confirmed each covered function still reads as the model says.

## Bounds and blind spots

Bounds: three Servers, two regions plus none, up to two Services, counts up to 2 in a region and 3 in all, one Volume with its writer fixed on `one`, one version bump per Service, and 4 changes (3 in Shared) per path. A bug that needs more of any of these is invisible.

Out of scope: execution failures, health checks and rollbacks (every operation completes); Bridge endpoint capacity (unbounded) and host ports (none); Global Services; dependencies between Services; a Server that doesn't answer or is Down; Drains; Volume moves; a Deploy by name; Interactive (non-runner) previews; an Ingress or hostname; the CLI and dashboard wording of a Wait. A green model says nothing about them.

## Coverage

The model calls these as they are, so a change to them is checked as written. Reread them when a diff touches them, to see whether a property's meaning moved:

- `core/crates/ployz/src/deploy/planning.rs`: `preview_deploy`, `plan_operations`, `assemble_plan` (the errors it blocks), `plan_one_service`, `place_service`, `spread_machines`, `eligible_machines`, `placement_error`, `configured_regions`.
- `core/crates/ployz/src/deploy/planning/placement.rs`: `Spread` (`of`, `wait_for`, `hold`, `standing`, `replicas`, `supplied`), `Quota`, `select_replicated`, `plan_replicated`, `withdraw`, `remove_unused`.
- `core/crates/ployz/src/deploy/planning/volumes.rs`: `refuse_kept_apart`, `reserve_shared`, `place`, `constrain_candidates`, `planned_volume_constraints`.
- `core/crates/ployz/src/deploy.rs`: `PlanError::blocked_kind`.
- `core/crates/ployz-core/src/domain/deploy.rs`: `DeployPreview::waits`, `WaitingReplicas`, `RegionWait`.

These the model copies, and they drift unless you keep them in step:

- `core/crates/ployz-core/src/config/lowering.rs`: the `placement` and `regions` fields `lower_snapshot` builds (`RegionPlacement::requested`).
- `core/crates/ployz-store/src/rules.rs`: the `VolumeInSeveralRegions` rule (`counts_menu`).
- `core/crates/ployz-store/src/deployment/outcomes.rs` `node_outcomes`, `deployment/mod.rs` `all_waiting` and `NodeStatus::advances`, `deployment/history.rs` `live` (`Verdict::advanced`, `applied`, `all_waiting`, `live`).

Sync commit: `b07db92772ca172331e8b60f74d02c3efd704c1b`, the `main` this model was checked against, with R4 (#4360) and R5 (#4334) in it. Code in the lists above that changed after it has not been rechecked.
