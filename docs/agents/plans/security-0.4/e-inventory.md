# Thread E inventory: where Cloud chooses Volume Run inputs

This file lists every place where Ployz Cloud chooses an input to a Volume Run, as of `main` at `00859a3dd4`. Each row says what that input reaches on a Machine and which slice in [e-plan.md](e-plan.md) closes it. Paths are relative to `core/crates/` unless they start with `dashboard/`. Cloud paths under `dashboard/` are relative to `dashboard/src/modules/`.

A row **crosses the bar** when a compromised Cloud can use it, under Device enforcement, to read secrets, run code of its choosing on a Server or take data off it. Other rows are inputs the design must still bind, so the closing slices name them. A row marked **residual** lets Cloud mislabel, reorder, delay or drop an approved action and nothing more. The plan names that residual once and no slice closes it.

## The kickoff anchors, checked against main

| Kickoff anchor | Where it is now | State |
| --- | --- | --- |
| `dashboard/volume-run/volume-run.server.ts:1333` | `commitMove` at `:1335`. One Postgres update sets the row to `phase: "committed", current: to`. `commitAndStart` (`:1366`) then sends B its start. | Open. The Move's point of no return is a Cloud row. No Machine records it. |
| `dashboard/volume-run/volume-run.inngest.ts:680` | `finishRelease` at `:633`, `releaseVolume` at `:552`. A Release goes forward to B when the row says committed and back to A when it does not. | Open. Cloud alone decides which side wins, and can ask for both. |
| `ployzd/src/volume_send.rs` | `serve` at `:38`, `send` at `:52`. A plain HTTP server on the Machine's management address. `GET /volume-send/<name>?target=…` streams `zfs send`. It checks nothing about the caller. | Open. The firewall (`ployzd/src/network/firewall.rs`, `management_policy`) accepts the port from every mesh peer, so any Machine in the Cluster can pull any snapshot of any Volume. |
| A driver gets both outcomes from one Move | `SetVolumeCopy` admits each step against the copy on that Machine and its lane floor (`ployzd/src/machine_api/local_volume_copy.rs:76`). B's promotion needs only a copy at the snapshot it is asked for. A's resume needs only its own read-only root. | Open. Nothing on B says A gave the Volume up, and nothing on A says it did. |

## Machine routes

Every row is `Container, Operator` scope in `ployz-core/src/rpc_catalog.rs`. None calls an `admit_operator_*` check, so Device enforcement changes nothing on any of them today.

| Route | Catalog | What Cloud chooses | Reaches | Closed by |
| --- | --- | --- | --- | --- |
| `SetVolumeCopy` with `want: Writer { start: Create }` | `rpc_catalog.rs:86` | `namespace`, the whole `resolved_spec`, `creation_key`, the Container `gen` and `rotation` | `start_volume_holder` (`ployzd/src/machine/local_machine/container.rs:284`) creates and starts that Container with `create_container_admitted`. The deploy approval check that `EnsureContainer` gets (`deploy_approval.rs:28`) is never called. A cut on the same route stops the Service first. **Crosses the bar** (#4041): code of Cloud's choosing runs with the Volume mounted. | E1 refuses it. E4 admits it again, bound to an approved Service payload. |
| `SeedVolume` | `rpc_catalog.rs:85` | `name`, `into`, `guid`, `gen` | A restore point of one Volume lands under any Docker Volume name on that Machine. The next approved Deploy that mounts that name reads another Service's data. **Crosses the bar** when that Service serves its files (#4041). | E1 refuses it. E7 admits it under an approval naming both Volumes. |
| `SetVolumeCopy` with `want: Mirror { from: Some }` | `rpc_catalog.rs:86` | Which Machine pulls, the source address, the snapshot | A copy of the Volume on any Machine in the Cluster, pulled from the address Cloud gives. With the send stream below, Cloud places customer data on Machines the customer never chose for it. | E3 |
| The send stream | `ployzd/src/volume_send.rs:44` | Nothing directly. The puller is the Volume plugin (`volume_plugin/transfer.rs`), told where to pull by the row above. | Bulk Volume data to whoever connects from a mesh address, for any snapshot, for as long as the copy exists | E3 binds it to a recipient grant and ends a Mirror's grant at its removal. E4 and E5 bound a Move's grant to that Move's snapshots. |
| `SetVolumeCopy` with `want: Mirror { from: None }` (the cut) | `rpc_catalog.rs:86` | Which Writer stops, the snapshot name, `drain` | The Service holding the Volume stops and its root turns read-only. A stop no person approved. Not on the bar. | E4 |
| `SetVolumeCopy` with `want: Writer { start: Kept }` | `rpc_catalog.rs:86` | Which copy becomes the Writer, and the `container_id` of the stopped Container that starts | The Machine checks only that the Container exists and that it manages it (`ployzd/src/machine_api/local_volume_copy.rs:352`), then starts it (`container.rs:284`). The Container can be one Cloud created and stopped before enforcement was on, or one that never mounted the Volume. **Crosses the bar** (#4041): Cloud's code runs, and the route skips the check an Operator's start of a Container gets. | E1 refuses it. E4 admits it only for a Container the approved cut's own record names. |
| `SetVolumeCopy` with `want: Writer { start: None }` | `rpc_catalog.rs:86` | Which copy becomes the Writer, at which snapshot and epoch | A second Writer when another copy still writes. Nothing starts. Not on the bar. It is the split E5 closes. | E4 (Move), E5 (handoff), E6 (Restore) |
| `SetVolumeCopy` with `want: Forget` | `rpc_catalog.rs:86` | Which slot or run snapshots are destroyed | Data loss. Not on the bar. The decision record keeps destructive storage requests bound to the reviewed workflow. | E6 |
| `SetVolumeCopy` with `want: Snapshot` | `rpc_catalog.rs:86` | Snapshot name | One more snapshot on the Writer. Harmless. | Stays open. E3 admits it under a Mirror's standing approval and never needs to. |
| `SetVolumeCopy` with `want: Floor` | `rpc_catalog.rs:86`, capability `:117` | `gen` | Raises the lane floor, so an older run's late steps refuse `Stale`. Every Deploy that places a Volume sends it (`dashboard/volume-run/volume-run.deploy.server.ts:205`). **Residual**: it can only delay or drop. | Stays open |
| `SetVolumeSize` | `rpc_catalog.rs:87`, capability `:119` | `maximum_bytes`, `allow_shrink` | The copy's quota. A shrink below what the copy references is refused by the Machine. Not on the bar. | E7 |
| `RemoveVolume` | `rpc_catalog.rs:83` | `name`, `force` | Deletes a Docker Volume. Thread C's inventory leaves it to thread F's coverage. | Not E. See `e-plan.md` Appendix A gap 8. |
| `PrepareVolumes` | `rpc_catalog.rs:81` | `specs` | Datasets created ahead of a Deploy. Thread C checks them at use. | Not E |
| `InspectVolumeCopy` | `rpc_catalog.rs:84` | `name` | A read of one copy's state. | Not E |

## The Move walk

| What Cloud chooses | Where | Reaches | Closed by |
| --- | --- | --- | --- |
| The destination Machine | `dashboard/volume-run/volume-run.server.ts:557` (`planVolumeRun`), `:722` (`requestVolumeRun`) | Where the Volume and its Service end up | E4. The approval names the source and the target. |
| The source address B pulls from | `:1534` (`managementAddress(machine.public_key)` from Cloud's Machine row), sent at `dashboard/volume-run/volume-run.inngest.ts:268` | Whose data B receives | E3. B derives the address from the Machine ID in the approval and its own peer record. |
| The Service B starts | `:1387` (`startWriter`). Cloud reads the spec off A's stopped Container with `inspectContainer`, rewrites `maximum_bytes` from its own row (`:1418`) and sets `pull_policy: "never"` (`:1429`). | The Container on B. See the first Machine route above. | E1, then E4 |
| `creation_key`, Container `gen`, `rotation` | `:1329`, `:1431` | Which slot the start takes, and whether it is routed | E4 pins the Service. The key, `gen` and rotation stay Cloud's. **Residual.** |
| The image copied from A to B | `:1320` (`copyImage`) | B's image store. Thread C made deploys content-addressed, so a planted tag runs nothing. | Not E |
| The commit | `:1335` (`commitMove`) | Whether a later Release goes forward or back | E5. A's own record decides. |
| Back or forward on failure | `:1668` (`failAndRelease`), `volume-run.inngest.ts:552`, `:633` | Which side starts again | E5. Release asks A for its decision. |
| The Writer epoch and the cut's snapshot name `f-<epoch>` | `:1587` (`takeEpoch`, from Postgres) | The number each Machine compares against its copy | E5 pins the number A cut at inside the receipt. Cloud still picks it. **Residual.** |
| The lane `gen` | The row's identity column, sent with every step (`:1241`) | Which run's steps a Machine still admits | Stays Cloud's. **Residual.** |
| `drain` | `DRAIN_MS`, sent with the cut | How long A waits before it stops the Service | Stays Cloud's. A short drain is an abrupt stop the approval already covers. **Residual.** |
| The warm rounds, their count and their snapshot names | `volume-run.inngest.ts:289` (`runRounds`) | How much B holds before the cut | Stays Cloud's. Fewer rounds lengthen downtime. **Residual.** |
| The Container A resumes | `volume-run.inngest.ts:565`, `:617`, `dashboard/volume-run/volume-run.deploy.server.ts:241`, each a `Start::Kept` with a `container_id` from Cloud | Which stopped Container starts on A. See the Kept route above. | E1, then E4 |
| Removing A's stopped Container after the Move | `:1441` (`removeKept`), `:1464` (`removeContainer`) | A's kept Container, the only way back to A | E5. A removes it only after it recorded the handoff. |
| Row ownership | `:1078` (`requireOwner`), `volume_run` table | Which Cloud worker may send steps | Stays Cloud's bookkeeping. **Residual.** |

## The other run kinds

| Kind | What Cloud chooses | Where | Reaches | Closed by |
| --- | --- | --- | --- | --- |
| Mirror | The Mirror's Machine and the source address | `volume-run.inngest.ts:306` (`mirrorVolume`) | A copy on a Cloud-chosen Machine | E3 |
| Sync (requested) | The reset: `Forget` on the Mirror and on the Writer's run snapshots, then a Mirror | `volume-run.inngest.ts:313` | A full re-copy | E3. The `Forget` writes a withdrawal, so the copy that follows needs a new Mirror approval. E6 puts the `Forget` under that approval. |
| Sync (automatic) | That it runs, every 15 minutes, and which Mirrors pull | `dashboard/volume-placement/volume-placement.inngest.ts:11`, `dashboard/volume-placement/mirror-sync.server.ts:273` | A pull on every Mirror with no person involved | E3. The Mirror's approval is standing, so a sync needs none. |
| Delete Mirror | The slot, its role, the typed confirmation Cloud says it saw. That the Mirror stays removed. | `volume-run.inngest.ts:332` (`deleteMirror`) | A copy destroyed. The confirmation lives in Cloud alone. Nothing on a Machine records the removal, so Cloud can mirror the Volume to that Machine again. | E3 writes a withdrawal before any `Forget` of a Mirror destroys its slot, approved or not, so the approvals that Machine admitted cannot bring the Mirror back. E6 puts the destruction under an approval that carries the confirmation. |
| Restore | The copy that becomes the Writer, and that writes after its snapshot are lost | `volume-run.inngest.ts:505` (`restoreVolume`) | A Writer at an older snapshot | E6 |
| Release | The plan (`ReleasePlan { sources, forgets, forward }`), which slots it destroys, which copy resumes | `volume-run.inngest.ts:552`, `:633` | A second Writer or a destroyed copy | E5 |
| Demote | That an older Writer turns read-only | `volume-run.inngest.ts:527` (`demoteVolume`) | Writes on the older copy stop. It only narrows. | E6, under the Restore's approval that made the newer Writer |
| Resize | `maximum_bytes`, grow or shrink, the order of Machines | `volume-run.inngest.ts:373`, `dashboard/volume-run/volume-size.server.ts:62` | Quotas | E7 |
| Seed | `into`, `guid` | `volume-run.inngest.ts:773` (`seedVolume`) | See `SeedVolume` above | E1, then E7 |
| Deploy lane | `Floor` on every copy. A promote with `Start::Kept` when it took over a cut (`fenceTaken`, `dashboard/volume-run/volume-run.deploy.server.ts:220`). A promote with `Start::Create` when it follows a committed Move (`fenceFollowed`, `:283`). | `dashboard/volume-run/volume-run.deploy.server.ts` | The same promotes as a Release | E5. Both are recovery steps of the Move that left them, under that Move's approval. |
| Orphan sweep | Which slots nobody owns | `volume-run.inngest.ts:804` (`sweepOrphans`) | Destroyed slots | E6. A sweep destroys only a slot whose Delete Mirror or Move approval already covered it. See Appendix A gap 7. |

## What Cloud keeps

Cloud keeps the `volume_run` row, the one-run-per-Volume lock, step retries and patience, lane `gen` numbering, epoch numbering and every message a person reads. A compromised Cloud can stall a run, end it early, run steps out of order or report a state no Machine holds. That is the residual. The Machines refuse every step the approval does not cover, so none of it takes data off a Server or runs anything on one.
