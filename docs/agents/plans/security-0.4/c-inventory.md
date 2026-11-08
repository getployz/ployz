# Thread C payload inventory

This file lists every field that reaches Docker, the Ingress Proxy, a pre-deploy hook, a mount or Sealed open on a Machine. It marks each field as approved, derived or Machine-local. Source is `main` at `297c636aec`. Paths are relative to `core/crates/` unless they start with `dashboard/`.

The bar decides the marks. A compromised Cloud must not be able to read secrets, run code of its choosing on a Server, take data off a Server, or add a Server or Device. A field is **approved** when changing it could cross that bar. The approval digest must cover it. A field is **derived** when Cloud picks it and the worst a wrong value does is mislabel, reorder, delay, drop or stop approved work. A field is **Machine-local** when the Machine computes or holds it and nothing from Cloud sets it.

## The kickoff anchors, checked against main

| Kickoff anchor | Where it is now | State |
| --- | --- | --- |
| `ployz-core/src/rpc_catalog.rs:73` | `ployz-core/src/rpc_catalog.rs:78`, the `EnsureContainer` row, still Operator scope | Moved. Under enforcement every new creation is refused today (`ployzd/src/machine/local_machine/restart_approval.rs:89`, called at `:121`). With enforcement off, Cloud still picks the image and every Docker field. |
| `ployzd/src/machine_api/local.rs:469` | `ployzd/src/machine_api/local.rs:529`, `ensure_container`. Admission runs at `:536` through `admit_operator_restart` (`restart_approval.rs:111`). | Moved. Same state as the row above. |
| `ployzd/src/docker/create.rs:157` | `container_create_body` at `ployzd/src/docker/create.rs:96`. The host config is at `:173`: `cap_add` `:190`, `pid_mode` `:192`, `privileged` `:200`, `sysctls` `:204`. The image is at `:242`. | Moved. Open with enforcement off. |
| `ployzd/src/docker/create.rs:401` | Host devices at `create.rs:378` (`path_on_host` `:383`). Device requests at `:389`. Bind Mounts in `docker_mounts` at `:424` (source `:443`). Volume driver config at `:487`. | Moved. Open with enforcement off. |
| `ployz-core/src/domain/volume.rs:364` | `VolumeDriver::parse` at `ployz-core/src/domain/volume.rs:364`. It refuses only the reserved `ployz` driver and passes options through. | Exact. Open, including under enforcement through `PrepareVolumes` (see Volumes below). |
| `ployzd/src/docker/volume.rs:155` | `create_volume` passes `driver_opts` at `ployzd/src/docker/volume.rs:158` | Moved by 3 lines. Open. |
| `ployz-core/src/domain/ingress.rs:75` | `caddy_service_spec` at `ployz-core/src/domain/ingress.rs:68` now only recognizes a legacy spec. | Closed by `3a032873ac`. The daemon pins both images (`ployzd/src/images.rs:11`). `admit_reserved_door` (`ployzd/src/machine/ingress.rs:16`) admits only the daemon's own door specs, and refuses `network` on any other Service. `translate_legacy_door` (`ployzd/src/machine/local_machine/container.rs:896`) places the daemon's doors in place of a client's Caddy spec. |
| `ployzd/src/ingress.rs:1144` | `write_certificate_files` inside `apply_caddy` at `ployzd/src/ingress.rs:1139` | Closed for the image swap. Certificate files reach only the pinned Caddy. Who may publish certificate material is thread X's question, not C's. |
| `ployzd/src/sealed_env.rs:56` | `open` at `ployzd/src/sealed_env.rs:56`, called from `create.rs:120` | Exact. Open with enforcement off. Under enforcement it is reachable only through a creation, which is refused today. |

Two things follow. First, with enforcement on, a Machine refuses every Operator deploy, so today the only way to deploy under enforcement is a Full caller such as a Device or the local socket. Thread C's job is to replace that blanket refusal with a digest check. Second, the ingress image swap no longer needs thread C.

## New Containers, the request

`EnsureContainerRequest` (`ployz-core/src/rpc.rs:604`) with `ContainerWant::Running { container: RunTarget::New(NewContainer) }` (`rpc.rs:622`, `:705`) is the only Operator route that creates a Service Container or a Hook Container. Admission is at `ployzd/src/machine/local_machine/restart_approval.rs:121`. Docker work starts at `ployzd/src/docker/lifecycle.rs:162`.

| Field | Where it is read | Mark | Why |
| --- | --- | --- | --- |
| `slot.namespace` | `rpc.rs:588` | Approved | It picks the Namespace the Container joins, its DNS search, and its Volume names. |
| `slot.service` | `rpc.rs:589` | Approved | It picks the approved Service entry the spec must match. |
| `slot.kind` (Service Container or pre-deploy hook) | `rpc.rs:590` | Approved | A hook runs `pre_deploy.command`. The hook must be in the approved spec. |
| `slot.creation_key` | `rpc.rs:591` | Derived | Idempotency key. A wrong one makes a duplicate copy of approved code. |
| `resolved_spec` | `rpc.rs:707` | Approved, field by field below | |
| `gen` | `rpc.rs:709` | Derived | Ordering label among copies. |
| `deployment_id` | `rpc.rs:712` | Derived | Correlation label. |
| `registry_auth` | `rpc.rs:715`, used at `lifecycle.rs:218` | Derived | Pull credential, used once and never stored. It cannot change the image once the image is pinned by digest. |
| `image_prepared` | `rpc.rs:718` | Derived | Skips the pull. Harmless once the image is pinned by digest. |
| `answer` | `rpc.rs:720` | Derived | When the Machine replies. |
| `rotation` | `rpc.rs:625` | Derived | Routing in or out. Outage only. |
| `approval` | `rpc.rs:611` | Not payload | Today a restart approval only. Thread C adds a deploy approval beside it. |

## New Containers, the resolved spec

`ResolvedServiceSpec` is at `ployz-core/src/domain/spec.rs:607`. `ResolvedServiceSpec::to_requested` (`spec.rs:772`) maps it back to the `RequestedServiceSpec` (`spec.rs:592`) that a Deploy Intent freezes. The approval digest is taken over that requested form, so a client can compute it from the frozen Attempt Target before any placement happens.

| Field | Reaches | Mark | Why |
| --- | --- | --- | --- |
| `service_id` | Docker label at `create.rs:134` | Derived | Cloud allocates it. A wrong value mislabels a copy. |
| `name` | Docker label at `create.rs:135` | Approved | Same check as `slot.service`. |
| `mode` | Placement and replica count in the client | Derived | Scale and spread only. Outage at worst. |
| `container` | Everything below | Approved | |
| `placement` | Client placement, Volume preparation | Approved | Part of the requested spec, so the digest covers it at no cost. The Machine does not check that its own labels satisfy it today. A copy placed on another listed Machine still runs only approved code. |
| `ports` | Docker port bindings at `create.rs:183`, Ingress routes | Approved | A new Host port or Ingress hostname can expose a private service, and Cloud can read plain variables such as a database password. That is a way to take data off a Server. |
| `mount_graph` | Mounts at `create.rs:151` and `lifecycle.rs:919` | Approved, see Volumes and Config Mounts | |
| `pre_deploy` | Hook Container body at `create.rs:105` | Approved, see hooks | |
| `update` | Rollout order in the client | Derived | Order and monitor window. Outage only. |

`ServiceContainerSpec` is at `ployz-core/src/domain/spec.rs:472`. Every field is approved. Some fields are outage-only, such as memory and the healthcheck. Splitting them out would make the digest rule longer and buy nothing.

| Field | Docker field (`create.rs`) | Mark |
| --- | --- | --- |
| `image` | `image` `:242`, pulled at `lifecycle.rs:218` by `prepare_image` (`ployzd/src/docker_image.rs:19`) | Approved. It must be a content-addressed reference (`name@sha256:…` or `sha256:<image id>`). A tag is Cloud-writable (see `c-decisions.md`). |
| `command`, `entrypoint` | `cmd` `:226`, `entrypoint` `:254` | Approved |
| `environment` (plain, Sealed and template values) | `env` `:219` after `sealed_env::open` `:120` | Approved. Plain values stay plain and Cloud can read them. |
| `sealed_identity` | Associated data for Sealed open | Approved, see Sealed open |
| `labels` | `labels` `:125`. The reserved `ployz.*` keys are refused at parse. | Approved |
| `hostname` | `hostname` `:243` | Approved |
| `extra_hosts` | `extra_hosts` `:193` | Approved |
| `cap_add`, `cap_drop` | `:190`, `:191` | Approved |
| `healthcheck` | `healthcheck` `:230` | Approved |
| `pull_policy` | `prepare_image` | Approved |
| `init`, `user`, `working_directory`, `tty`, `open_stdin` | `:177`, `:214`, `:248`, `:217`, `:218` | Approved |
| `privileged` | `:200` | Approved |
| `pid_mode` | `:192` | Approved |
| `resources.devices` | `devices` `:378` (host path `:383`) | Approved. A host disk mapped into a Container takes data off a Server. |
| `resources.device_reservations`, `ulimits`, `shared_memory_bytes`, CPU and memory | `docker_resources` `:373` | Approved |
| `stop_timeout_secs`, `restart` | `:268`, `:184` | Approved |
| `sysctls` | `:204` | Approved |
| `network` | `network_mode` `:178` | Approved. The Machine also refuses it outside the two door Services (`ployzd/src/machine/ingress.rs:25`). |

## Volumes

Bind Mounts and named Volumes reach Docker through `docker_mounts` (`ployzd/src/docker/create.rs:424`). A named Volume that does not exist is created by Docker from the mount's driver config (`create.rs:487`). A named Volume that already exists is used as it is, whatever driver and options it was created with.

| Field | Where | Mark | Machine-side check needed |
| --- | --- | --- | --- |
| Bind `machine_path`, `create_machine_path`, `propagation`, `recursive` | `ployz-core/src/domain/volume.rs:46`, `create.rs:436` | Approved | None beyond the digest. `MachinePath` accepts any absolute path (`ployz-core/src/value.rs:508`). A person may approve `/`. |
| External `name` | `volume.rs:56`, `create.rs:466` | Approved | At use, refuse a Volume that carries Ployz's managed label. External means made outside Ployz. Under enforcement, `PrepareVolumes` is the only way Cloud can make a named Volume, and those carry the label. |
| Ordinary `name`, `driver`, `options`, `labels` | `volume.rs:58`, `volume.rs:352` | Approved | At use, an existing Volume must match the approved declaration through `VolumeSource::matches_managed_volume` (`volume.rs:221`). Only `PrepareVolumes` calls that today (`ployzd/src/docker/volume.rs:375`). |
| Provisioned `name`, `maximum_bytes`, `labels` | `volume.rs:65` | Approved | Same check. It refuses a Volume of that name made with another driver. |
| Tmpfs `size_bytes`, `mode`, `options` | `volume.rs:74`, `create.rs:499` | Approved | None |
| Mount `target`, `read_only`, `no_copy`, `subpath` | `volume.rs:26` | Approved | None |
| Physical Volume name and owner labels | `ScopedVolumeSource::physical_name` `volume.rs:104` | Machine-local, from the approved Namespace | None |

The kickoff's driver-option bypass works like this. Cloud calls `PrepareVolumes` (`ployzd/src/machine/local_machine/container.rs:97`), which enforcement does not gate. It makes an Ordinary Volume with the `local` driver and `o=bind,device=/` under the physical name an approved spec will later mount. The approved Container then mounts the host root. The digest alone does not stop this, because the approved spec names the Volume, not its existing contents. The at-use check above closes it. `PrepareVolumes` itself stays ungated, since making a Volume nobody mounts crosses no part of the bar.

## Config Mounts

| Field | Where | Mark |
| --- | --- | --- |
| `ConfigSpec.name`, `ConfigSpec.content` (the file bytes, inline, variables already resolved) | `ployz-core/src/domain/spec.rs:85` | Approved. The bytes are in the spec, so the spec digest is the content hash the plan asks for. |
| `ConfigMount.config_name`, `target`, `uid`, `gid`, `mode` | `spec.rs:101` | Approved |
| The file on disk and its path | `materialize_config` (`ployzd/src/docker/spec_store.rs:499`), named by `config_identity` (`spec_store.rs:624`), the SHA-256 of content, uid, gid and mode. Bind-mounted read-only at `ployzd/src/docker/lifecycle.rs:919`. | Machine-local. The daemon writes it from the approved bytes. |

No separate Config hash field is needed.

## Pre-deploy hooks

A hook is a New creation with `slot.kind = PreDeployHook`. `container_create_body` merges the hook's fields over the Service's (`create.rs:105`).

| Field | Where | Mark |
| --- | --- | --- |
| `pre_deploy.command` | `create.rs:226` | Approved |
| `pre_deploy.environment` | merged at `create.rs:116`, opened at `:120` | Approved |
| `pre_deploy.privileged` | `create.rs:201` | Approved |
| `pre_deploy.user` | `create.rs:214` | Approved |
| `pre_deploy.timeout_millis` | the client's hook wait | Approved |
| `PLOYZ_HOOK_PRE_DEPLOY`, empty entrypoint, disabled healthcheck, restart `no` | `create.rs:117`, `:254`, `:232`, `:184` | Machine-local |

A hook that already ran is approved code. Running it again does not cross the bar. The plan still asks that redeploying an approved revision reruns no hooks as fresh work. The Machine's existing consumption log (`restart_approval.rs:250`) can record one hook creation per operation and Service. That is the only consumption C needs.

## Fields the Machine adds at creation

These are Machine-local. Nothing in a request sets them.

- `PLOYZ_MACHINE_ID` in the environment (`create.rs:119`).
- The reserved labels for managed, Namespace, Service ID, Service name, hook, bridge IPv6, Sealed record, generation, deployment and creation key (`create.rs:131` to `:148`, `lifecycle.rs:207` to `:216`).
- DNS server, search domain and `ndots` (`create.rs:174`).
- The network mode, which is the bridge or `container:<holder>` resolved by the daemon (`lifecycle.rs:178`).
- The log driver (`create.rs:182`).
- The bridge address (`lifecycle.rs:902`).
- Ingress sysctls, only for the reserved Namespace (`create.rs:49`).
- The addresses of host interfaces for a prefix Host bind (`create.rs:162`).
- The Sealing Key directory and the opened plaintext (`create.rs:120`).

## Lifecycle on existing Containers

`Stopped`, `Gone`, `Out`, `Held` and `Running { Existing }` act on a Container that already exists. They map to `RestartStep` (`ployz-core/src/authority/restart.rs:172`). Under enforcement each needs a restart approval today (`restart_approval.rs:124`). Hold, Stop and Start are approvable, but no `WorkflowKind` permits Out or Remove (`restart.rs:32`). A Deploy's rollout needs all five on the old copies of the Services it replaces, and on Services it prunes.

| Field | Mark | Why |
| --- | --- | --- |
| `container` locator | Derived | It picks which copy stops. Outage only. |
| `drain`, `stop` options, `anonymous_volumes` | Derived | Timing, and whether anonymous Volumes go. Data loss is not on the bar. |
| `allow_empty`, `expect`, `run` | Derived | Routing. Outage only. |

None of these crosses the bar. Thread C admits them under a deploy approval for the Namespace that approval names, inside its window, with no per-step consumption.

## Ingress

| Input | Where | Mark |
| --- | --- | --- |
| Holder and Caddy images | `ployzd/src/images.rs:11` | Machine-local, pinned by digest |
| Door specs | `DoorSpecs::built_here`, checked by `admit_reserved_door` (`ployzd/src/machine/ingress.rs:16`) | Machine-local |
| `SetIngressDoorsRequest.doors` (open or closed) | `ployz-core/src/rpc.rs:1604`, handler `ployzd/src/machine_api/local.rs:1063` | Derived. Doors expose only routes to approved ports. Enforcement refuses the route today through `admit_operator_deploy`. That refusal is broader than the bar needs. See the handoffs in `c-plan.md`. |
| `proxy_service` | `rpc.rs:1608` | Derived |
| Routes (hostname to Container port) | built from the Service Containers' approved `ports` | Approved, through the spec digest |
| Certificate material | `PublishCertificateMaterial` (`local.rs:980`), written at `ployzd/src/ingress.rs:1144` | Thread X. Cloud already holds the ACME and DNS credentials. |

## Sealed open

`sealed_env::open` (`ployzd/src/sealed_env.rs:56`) opens every Sealed part of a spec's environment with the Machine's Sealing Key. Its only Docker-bound caller is `container_create_body` (`create.rs:120`). The Build path opens through `open_for_build` (`ployzd/src/build/sealed.rs:18`), which is thread D's. Log scrubbing opens values too (`ployzd/src/docker/stream.rs:182`), but only to hide them in output, so the plaintext never leaves the Machine.

| Input | Where | Mark |
| --- | --- | --- |
| `SealedValue.sealed`, `scope`, `key` | `ployz-core/src/sealed.rs:49` | Approved, inside `environment` |
| `SealedIdentity.organization`, `environment` (a Cloud Environment UUID), `service` | `sealed.rs:73` | Approved |
| Associated data | `SealedValue::associated_data` (`sealed.rs:130`), built from the identity, scope and variable name | Derived on the Machine from approved fields |
| Sealing Key | the Machine's key directory | Machine-local |

Today, "replacement code opens valid Sealed values" works because the ciphertext and identity are checked but the image and command beside them are not. Once a creation must match an approved digest, the ciphertext, its identity, the image, the command, the mounts and the privileges all sit under one digest. Sealed open is then bound to that digest without changes inside `sealed_env`.

Approval by a session grant leaves one gap. A grant names a Namespace (`ployz-core/src/authority/session.rs:51`). A Sealed identity names an Environment UUID. A Machine cannot tell whether a payload approved under a `fix-api` grant carries `production` ciphertext. That is a cross-Environment secret read inside a live session, and the session's scope should prevent it. `c-plan.md` slice C4 closes it.

## Other mutating routes that touch C's fields

| Route (`rpc_catalog.rs`) | Gated under enforcement today | On the bar | Owner |
| --- | --- | --- | --- |
| `PrepareVolumes` `:81` | No | Only through a later mount. Closed by the at-use Volume check. | C |
| `PullImage` `:91`, `PullImageFromMachine` `:92`, `EnsureImageIngest` `:90`, `MintBuildGrant` `:93` | No | They write the image store under any tag. Closed for deploys by content-addressed images. | C for deploys, D for grants |
| `UpdateContainerLimits` `:79` | No | No. CPU and memory only (`ployz-core/src/domain/spec.rs:339`). | F coverage |
| `RemoveVolume` `:83`, `PruneImages` `:89` | No | No. Deletion is not on the bar. | F coverage |
| `SetIngressDoors` `:98` | Refused outright | No | F coverage |
| `Build` `:111` | **No** | **Yes.** A Cloud-chosen build runs `RUN` steps on the Server, with the Build's Sealed values opened as secret files (`ployzd/src/build.rs:40`, `ployzd/src/build/sealed.rs:18`). | D, with F's coverage test |
| `Exec` `:110` | Full scope only | Refused to Operator | A |

The `Build` row is the one open bar crossing this inventory found outside thread C's files. It is already inside thread D's scope ("builds and caches"). `c-decisions.md` makes C's cache decision depend on it.
