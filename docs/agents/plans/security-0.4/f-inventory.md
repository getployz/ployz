# Thread F inventory: route classes, automatic effects, unsigned paths and the switch

This file lists, as of `main` at `02aaa926b5`, every Machine route with the authorization class [f-plan.md](f-plan.md) gives it. It also lists every automatic effect and every Cloud path still sent unsigned, plus every place the enforcement switch lives. Paths are relative to `core/crates/` unless they start with `dashboard/`. "Today" means what an Operator's call does on a Machine with Device enforcement on.

A route **crosses the bar** when a compromised Cloud can use it, under enforcement, to read secrets, run code of its choosing on a Server, take data off it, or add a Server or Device. A route marked **residual** lets Cloud mislabel, reorder, delay or drop and nothing more. The plan names that once.

## Every route

`Signed(…)` lists the statements that admit an Operator's call. `Signed()` with nothing listed means no statement admits it yet, and only a Full caller can call it. The class column is the class F1 writes. A different class later names the slice that moves it. A `Bound` row succeeds unsigned by design, so "Live matrix" below, not this table alone, is what the live lanes expect.

### Reads

These change nothing on the Machine. Class `Read`. `RetainedEnv` answers opened values only as ciphertext (`ployzd/src/sealed_env.rs:773`). Logs and metrics are data Cloud already sees, a residual the decision record names.

`DescribeContract`, `Inspect`, `PendingJoins`, `ListMachines`, `ListContainers`, `InspectContainer`, `InspectStorage`, `ListVolumes`, `InspectVolumeCopy`, `ListImages`, `InspectBuildGrant`, `ReadRoutes`, `ReadRoutedAddresses`, `ReadClusterName`, `MetricHistory`, `RetainedEnv`, `LogHistory`, `TailLogs`, `RuntimeWatch`.

### Full scope

Refused at dispatch for any caller but the local socket or a Device's own connection (`ployzd/src/machine_api/mod.rs:212`). Class `Full`.

`WrapSealingKey`, `RecoverSealingKey`, `SetDevice`, `SetPasskey`, `RemoveTrustedKey`, `SetDeviceEnforcement`, `Exec`.

`SetDeviceEnforcement` keeps `Full`. F6 refuses `enforced: false` once a Machine is activated, and F7 makes "off" unreachable.

### Mutating routes

| Route | Capability | Today under enforcement | Class at F1 | Later |
| --- | --- | --- | --- | --- |
| `Enroll` | `ployz.machine.enroll.v1` | A member needs an approved capsule, and a founder is refused (`local_machine.rs:811`). **A fresh daemon is not enforcing**, so it takes Cloud's Devices when the capsule is withheld (`machine/store.rs:551`). A1's accepted residual. | `Pending("F7")` | `Signed(Join)` at F7, plus a first key for a founder (gap 6) |
| `EnrollForRecovery` | `ployz.machine.enroll-for-recovery.v1` | A fresh daemon founds a Cluster awaiting recovery. It holds no key until `RecoverSealingKey`, which is `Full`. | `Pending("F7")` | As `Enroll`'s founder at F7 |
| `Register` | `ployz.machine.register.v1` | Needs a join approval. Publishes only after the joiner's confirmation (#4025). | `Signed(Join)` | |
| `ApproveJoin` | `ployz.machine.join.approve.v1` | Carries the Device's signature | `Signed(Join)` | |
| `ShareSealingKey` | `ployz.sealing.share.v1` | Needs a join approval (`local_machine/sealing.rs:162`) | `Signed(Join)` | |
| `GenerateSealingKey` | `ployz.sealing.generate.v1` | Only when the Cluster publishes no key. The key stays on the Machine. | `Bound("the key never leaves the Machine")` | |
| `InstallSealingKey` | `ployz.sealing.install.v1` | Opens only a key sealed by an accepted Machine's WireGuard key (`sealing.rs:262`) | `Bound("Cloud holds no Machine's key to seal from")` | |
| `SetManagementClient` | `ployz.machine.set-management-client.v1` | Refused from the mesh under enforcement. Through Cloud's own connection it mints another Operator credential. Device slots are reserved. | `Bound("mints only Operator authority Cloud already has")` | |
| `SetPushTarget` | `ployz.machine.set-push-target.v1` | Sets where crash events go | `Bound("crash events are data Cloud already reads")` | |
| `SelectEndpoint` | `ployz.network.select-endpoint.v1` | Chooses a WireGuard path. Peers stay key-bound. | `Bound("residual: delay or drop")` | |
| `UpdateMachine` | `ployz.machine.update.v1` | Name, labels, endpoints and policy flags | `Bound("residual: mislabel; approvals bind IDs")` | |
| `SetClusterName` | `ployz.cluster.name.v1` | A label | `Bound("residual: mislabel")` | |
| `RequestMachineUpgrade` | `ployz.machine.upgrade.request.v1` | Any channel, or an exact version either channel resolves to (`installer/upgrade.rs:159`) | `Bound("signed releases only, forward only")` | `Signed(Upgrade)` at F2 |
| `Leave` | `ployz.machine.leave.v1` | **Admitted.** Removes every managed Container, departs storage and resets the daemon to a fresh, unenforced one (`local_machine.rs:1141`, `machine/store.rs:940`). | `Signed()` | `Signed(Teardown)` at F5 |
| `Forget` | `ployz.machine.forget.v1` | **Admitted.** Deletes another Machine's row, and that Machine fences itself the same way, keeping its copies sealed (`local_machine.rs:1107`, `forgotten_fence.rs:238`). | `Signed()` | `Signed(Teardown)` at F5 |
| `EnsureContainer` | `ployz.container.ensure.v1` | Creations need a deploy approval. Lifecycle steps need a restart, deploy, proposal or rule approval. | `Signed(Deploy, Restart, Proposal, Rule)` | `Teardown` added at F5 |
| `UpdateContainerLimits` | `ployz.container.limits.update.v1` | CPU and memory limits | `Bound("residual: a limit can slow or stop a Service, a drop")` | |
| `PrepareVolumes` | `ployz.storage.prepare.v1` | Creates datasets ahead of a Deploy. Thread C checks a Volume when a Container mounts it. | `Bound("checked at mount by C")` | |
| `RemoveVolume` | `ployz.volume.remove.v1` | **Admitted.** Deletes a Docker Volume. Data loss, not a bar crossing. | `Pending("E6")` | `Signed(Volume)` at E6 (gap 5) |
| `SeedVolume` | `ployz.volume.seed.v1` | Refused (E1) | `Signed(Volume)` | Admitted under approval at E7 |
| `SetVolumeCopy` | `ployz.volume.copy.set.v1` | A Writer want with a start is refused (E1). Pulls, cuts, promotes and forgets are admitted. `Floor` and `Snapshot` are residuals. | `Pending("E3 to E6")` | `Signed(Volume)` at E6 |
| `SetVolumeSize` | `ployz.volume.size.set.v1` | Admitted. A shrink below what the copy holds is refused by the Machine. | `Pending("E7")` | `Signed(Volume)` at E7 |
| `PruneImages` | `ployz.image.prune.v1` | Removes unreferenced images | `Bound("residual: the next deploy pulls again")` | |
| `EnsureImageIngest` | `ployz.image.ingest.ensure.v1` | Opens an image push. An image runs only when a deploy approval names its digest. | `Bound("content-addressed; checked at creation by C")` | |
| `PullImage` | `ployz.image.pull.v1` | As above | `Bound("content-addressed; checked at creation by C")` | |
| `PullImageFromMachine` | `ployz.image.pull-from-machine.v1` | As above | `Bound("content-addressed; checked at creation by C")` | |
| `MintBuildGrant` | `ployz.build.grant.mint.v1` | Refused unless it carries a rule and proof (D1, D3) | `Signed(Rule)` | `BuildApproval` added at D6 |
| `Build` | `ployz.build.v1` | Refused (D1) | `Signed()` | |
| `PublishCertificateMaterial` | `ployz.certificates.publish.v1` | Admitted. Sets the key the Ingress Proxy serves for a hostname. | `Bound("DNS and ACME authority Cloud holds; thread X")` | |
| `SetIngressDoors` | `ployz.ingress.doors.v1` | Refused (`machine_api/local.rs:1167`) | `Signed()` | `Bound("daemon-pinned images; tunnel token is thread X's")` at F3 |
| `SetPrivateAccess` | `ployz.private-access.v1` | Refused (`machine_api/local.rs:1179`) | `Signed()` | `Signed(PrivateAccess)` at F3 |

## Live matrix

The attacker harness never assumes an unsigned call is refused. It runs F1's `CASES` (f-plan.md, "Authorization classes"), which give each branch its own expected answer. "Refused" means refused naming the capability with nothing changed. "Bounded" means served with the check holding. "Served" means served. The column is the answer after F7. A `Pending` row is Served until the slice named in "Every route".

Routes that carry several mutations need one case per branch:

| Route | Branch | Expected from Cloud, unsigned, on an enforcing Machine |
| --- | --- | --- |
| `Enroll` | Member with no capsule | Refused (F7) |
| `Enroll` | Member with an approved capsule | Served |
| `Enroll` | Founder | Refused until its first key arrives over local administration (F7, gap 6) |
| `EnsureContainer` | Create | Refused. Needs a deploy approval. |
| `EnsureContainer` | Start, stop, restart | Refused. Needs a restart, deploy, proposal or rule approval. |
| `EnsureContainer` | `Out`, `Remove` | Refused. Needs a deploy approval, or a teardown from F5. |
| `RequestMachineUpgrade` | The release the signed channel resolves to | Served (F2) |
| `RequestMachineUpgrade` | Another channel, an exact version off the channel, or no statement | Refused (F2) |
| `RequestMachineUpgrade` | A downgrade | Refused |
| `SetIngressDoors` | The daemon's pinned door image | Bounded (F3) |
| `SetIngressDoors` | Any other image | Refused (F3) |
| `SetPrivateAccess` | On | Refused (F3) |
| `SetPrivateAccess` | Off | Served (F3) |
| `GenerateSealingKey` | The Cluster publishes no key | Bounded. The key stays on the Machine. |
| `GenerateSealingKey` | The Cluster already publishes one | Refused |
| `InstallSealingKey` | Sealed by an accepted Machine's key | Bounded |
| `InstallSealingKey` | Sealed by any other key | Refused |
| `SetManagementClient` | From Cloud's own connection | Bounded. Device slots are untouched. |
| `SetManagementClient` | From the mesh, or naming a Device slot | Refused |
| `SetVolumeCopy` | A Writer want with a start | Refused (E1) |
| `SetVolumeCopy` | Pull, cut, promote, forget | Refused from E6 |
| `SetVolumeCopy` | `Floor`, `Snapshot` | Served. A residual, as "Every route" says. |
| `SetVolumeSize` | Grow, or a shrink | Refused from E7. A shrink below what the copy holds is refused at any slice. |
| `MintBuildGrant` | With a rule and its proof | Served |
| `MintBuildGrant` | Without | Refused |
| `Leave`, `Forget` | No statement | Refused (F1) |
| `Leave`, `Forget` | A teardown naming another Machine | Refused (F5) |
| `PullImage`, `PullImageFromMachine`, `EnsureImageIngest` | Any image | Bounded. A following unsigned `EnsureContainer` create with that image is refused. |
| `SetDeviceEnforcement` and every other `Full` row | From the mesh | Refused at dispatch |

Every other `Bound` row has one Served case. Every other `Signed` row has one Refused case.

## Automatic effects

Each runs with no person present. "Class" is the route class it rides, or "local" for work the daemon does without an RPC.

| Effect | Who drives it | Route | Under enforcement today | After F |
| --- | --- | --- | --- | --- |
| Ingress startup and repair | Cloud (`dashboard/ingress/doors.server.ts`), and a joiner's own daemon during global catch-up | `SetIngressDoors` | Refused through Cloud | `Bound` at F3 |
| Certificate renewal | The daemon's Caddy | Local | Runs | Unchanged. F3 lane 4 and F6 lane 9 check it with Cloud stopped. |
| Certificate material for Cloud-managed hostnames | Cloud (`dashboard/cluster-domain/sync.server.ts`) | `PublishCertificateMaterial` | Runs | `Bound`, thread X |
| Image pruning | The deploy runner after a Deploy | `PruneImages` | Runs | `Bound` |
| Preview cleanup | Cloud on pull request close (`dashboard/machines/namespace-cleanup.*`) | `EnsureContainer` (`Out`, `Remove`), `RemoveVolume` | Container removal is refused. Volume removal runs. | Under D7's preview rule at F5 (gap 4) |
| Deleted Environment cleanup | Cloud (`dashboard/machines/namespace-cleanup.*`) | As above | As above | Namespace teardown at F5 |
| Automatic Mirror sync | Cloud (`dashboard/volume-placement/mirror-sync.server.ts`) | `SetVolumeCopy` | Runs unsigned | Mirror's standing approval (E3) |
| Orphan sweep | Cloud (`sweepOrphans`) | `SetVolumeCopy` `Forget` | Runs unsigned | The approval that orphaned the slot (E6) |
| Volume recovery after a stalled Move | Cloud (`releaseVolume`, `finishRelease`) | `SetVolumeCopy` | A start is refused (E1) | A Release under the Move's own approval (E5) |
| Global catch-up on a joiner | The joining CLI over the joiner's local socket (`ployz/src/global_catch_up.rs`) | `EnsureContainer` as a Full caller | Runs | Unchanged. It is a Full caller on the joiner itself. |
| Automatic upgrades | Cloud's rollout (`dashboard/server-upgrade/server-upgrade.server.ts:212`) | `RequestMachineUpgrade` | Runs along any channel | Along the signed channel at F2. Never 0.3 to 0.4 at F6. |
| Forgotten-Machine fence | The forgotten Machine's own daemon, on its row's removal | Local, after `Forget` | Runs on any `Forget` | Only after a signed `Forget` reaches another Machine (F1, F5) |

## Cloud paths still sent unsigned

Each is refused on an enforcing Machine today, so each is a flow a customer loses by turning enforcement on.

| Path | Where | Signed by |
| --- | --- | --- |
| **Redeploy** | `dashboard/config-store/**` | F4 |
| Rollback of an image Service | `dashboard/service-act/**`, `dashboard/config-store/**` | F4 |
| Rollback of a Git Service | as above | D6's `BuildApproval`, wired in F4 |
| **Retry** | `dashboard/config-store/**`. The Store has no approval field yet. | F4 |
| A Sync's deploy | `dashboard/config-store/**` | F4 |
| A new branch's first deploy | `dashboard/config-store/**` | F4 |
| Environment deletion, preview close | `dashboard/machines/namespace-cleanup.*` | F5 |
| Server removal and forgetting | `dashboard/machines/machine-removal.*`, `forget-servers.*` | F5 |
| Upgrades | `dashboard/server-upgrade/**` | F2 |
| Ingress doors, Private Access | `dashboard/ingress/**`, Private Access settings | F3 |
| Move, Mirror, Restore and other Volume Runs | `dashboard/volume-run/**` | E3 to E8 |
| The old dispatched GitHub build workflow | `dashboard/github/**`, `dashboard/config-store/store-github*` | Deleted at F7, per d-plan's handoff |

## Where the switch lives

F7 deletes each `false` branch, or the field's use, below. Found with `rg -l device_enforcement core/crates` outside tests, and `rg -il 'enforce' dashboard/src/modules` outside tests.

| File | What reads the switch |
| --- | --- |
| `ployzd/src/machine/mod.rs`, `ployzd/src/machine/record_wire.rs` | The record field and its wire form. F7 keeps the read on first start only. |
| `ployzd/src/machine/store.rs` | `member`'s capsule-less branch (`:551` to `:559`), `install`, `set_device_enforcement` (`:902`) |
| `ployzd/src/machine_api/mod.rs` | Full scope for a mesh caller when not enforcing (`:185`) |
| `ployzd/src/machine_api/local.rs` | Each `from_operator` check that passes when not enforcing |
| `ployzd/src/machine/local_machine.rs` | Founder refusal (`:811`), join confirmation (`:872`), `register` (`:920`) |
| `ployzd/src/machine/local_machine/{restart_approval,deploy_approval,volume_approval,sealing,join_approval,container}.rs` | Each `admit_*` that returns early when not enforcing |
| `ployzd/src/management/{mod,build_grant}.rs` | Grant admission and serving |
| `ployzd/src/corrosion/store.rs` | The peer filter (`:736`) and the runtime report (`:85`) |
| `ployz-core/src/{rpc,rpc_catalog,domain/machine}.rs` | `SetDeviceEnforcementRequest.enforced`, the capability, the runtime field |
| `ployz/src/handlers/device.rs` | `enforce --off` (`:91`) |
| `ployz/src/handlers/deploy/approve.rs` | Signing only when a Machine enforces |
| `dashboard/src/modules/{service-act,approval-session,approvals,proposals,volume-run,runtime,machines}/**` | Choosing the unsigned path for a Machine that does not enforce |
