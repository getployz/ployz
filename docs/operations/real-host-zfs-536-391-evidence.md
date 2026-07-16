# Real-Host ZFS Certification Evidence — #536 / #391

This is a fill-in record, not evidence that a run occurred. Copy it into the
immutable evidence root for the sealed candidate and replace every
placeholder. Do not mark an item complete from recollection or from another
candidate.

## Candidate and custody

- Run id: `<id>`
- Run window UTC: `<start> → <finish>`
- Operator: `<name>`
- Evidence root: `<durable absolute location>`
- Evidence root digest/inventory: `<location>`
- Immutable release tag: `<v-tag>`
- Runtime SHA resolved from tag: `<40-lowercase-hex>`
- `git merge-base --is-ancestor 2f754ab5cff785fd67cf4c83231f4025ec6ad8ee <runtime-sha>`:
  `<command/output>`
- Release asset verification: `<command/output/location>`
- Alpha channel equality: `<channel fields/output/location>`
- Installed core tag/manifest: `<release.env evidence>`
- Installed edge tag/manifest: `<release.env evidence>`
- Clean harness SHA: `<sha; git status evidence>`
- Harness/script SHA comparison with runtime SHA: `<recorded comparison>`
- Success marker and exit status: `<location; status>`

Do not record secrets. Redactions must preserve the fact that authenticated
and public-release paths were used.

## Host and access matrix

| Role | Provider id | Provider image | OS/version | Native arch | Public/private IP | Guest firewall | Provider firewall ids | SSH key fingerprint | Rescue-console proof |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| core | `<id>` | `<image>` | `Rocky 9 <version>` | `amd64/x86_64` | `<addresses>` | `<firewalld evidence>` | `<ids>` | `<fingerprint>` | `<location>` |
| edge | `<id>` | `<image>` | `Ubuntu 24.04 <version>` | `arm64/aarch64` | `<addresses>` | `<UFW evidence>` | `<ids>` | `<fingerprint>` | `<location>` |

- Ephemeral key generated/loaded UTC: `<evidence>`
- Provider firewall source restrictions: `<rules/evidence>`
- Destructive consent guards: `<redacted invocation showing all exact names>`
- Initially empty absolute evidence directory: `<proof>`

## Artifact inventory

| Required artifact | Present | SHA-256 or immutable reference | Notes |
| --- | --- | --- | --- |
| `metadata.env` | [ ] | `<digest>` | `<redactions>` |
| `live-alpha.env` | [ ] | `<digest>` | `<channel equality>` |
| `transcript.log` | [ ] | `<digest>` | `<notes>` |
| `sealed-harness.sh` | [ ] | `<digest/harness SHA>` | `<notes>` |
| `commands.log` | [ ] | `<digest>` | `<notes>` |
| `recovery.txt` | [ ] | `<digest>` | `<notes>` |
| `zfs-module-recovery.env` | [ ] | `<digest>` | `<redactions>` |
| numbered phase logs | [ ] | `<inventory digest>` | `<range/notes>` |
| `sha256sums` | [ ] | `<digest>` | `<verification output>` |
| release asset verifier output | [ ] | `<digest/location>` | `<notes>` |
| alpha channel response | [ ] | `<digest/location>` | `<notes>` |
| provider console/rescue evidence | [ ] | `<digest/location>` | `<notes>` |
| teardown inventory | [ ] | `<digest/location>` | `<notes>` |

## #536 acceptance matrix

| Acceptance item | Assertion | Commands/output | Timing UTC/duration | Evidence location | Result |
| --- | --- | --- | --- | --- | --- |
| Public sealed candidate | Immutable tag resolves exactly to runtime SHA; assets verified; alpha promoted; both hosts' installed tags/manifests recorded; runtime contains `2f754ab5cff785fd67cf4c83231f4025ec6ad8ee` | `<evidence>` | `<time>` | `<location>` | [ ] |
| Fixed mixed-architecture pair | Rocky 9 amd64 core and Ubuntu 24.04 arm64 edge match native kernels and release assets | `<evidence>` | `<time>` | `<location>` | [ ] |
| Explicit destructive consent | All four exact environment guards accepted; evidence root was absolute and empty | `<evidence>` | `<time>` | `<location>` | [ ] |
| Storage preparation | Public operation id reaches terminal success; storage is Ready; `/var/lib/ployz/prepared-storage.json`, owned backing file, pool, dataset root, capacity, and mountpoint agree | `<operation/commands>` | `<time>` | `<location>` | [ ] |
| Real Provisioned Volume | PostgreSQL pin names Rocky; `volume list`, Docker mount and recovery labels, and `zfs list` identify the same quota-bearing `/var/lib/ployz/volumes/...` dataset | `<commands>` | `<time>` | `<location>` | [ ] |
| Pre-reboot row | Unique row is written and read; row marker, dataset, container, pool health, and testimony captured | `<commands>` | `<time>` | `<location>` | [ ] |
| Pool import ordering | Normal reboot returns without manual import; ZFS precedes Docker workload start | `<systemd/zpool evidence>` | `<time>` | `<location>` | [ ] |
| Reboot persistence | Same dataset and row return; no plain-directory or empty-database substitution | `<commands>` | `<time>` | `<location>` | [ ] |
| Reversible quarantine | Exact running-kernel loadable module path, hash, owner, mode, successful initramfs inventory, pre-mutation `lsmod`/ZFS units, verified backup, and local emergency recovery steps recorded before mutation | `<commands>` | `<time>` | `<location>` | [ ] |
| Module-break reboot | Reboot occurs with ZFS absent from module search paths and stale initramfs recovery excluded | `<commands>` | `<time>` | `<location>` | [ ] |
| Loud workload failure | Database is not running or replaced against an empty directory; failure is visible and bounded | `<Docker/mount/query evidence>` | `<time>` | `<location>` | [ ] |
| Typed testimony | Fresh testimony is `Unavailable { ZfsModuleMissing }` / CLI `unavailable zfs-module-missing` | `<commands/output>` | `<time>` | `<location>` | [ ] |
| Stranded-pin alarm | Alarm names exact machine, namespace, volume, and dataset/pin reason | `<commands/output>` | `<time>` | `<location>` | [ ] |
| Control-plane independence | Control services remain active and bounded non-storage commands answer during storage failure | `<commands/output>` | `<time>` | `<location>` | [ ] |
| Non-destructive failure | Backing-file resolved path and identity, descriptor hash/content, complete Docker label map, operation evidence, dataset identity, and row marker are preserved | `<commands/output>` | `<time>` | `<location>` | [ ] |
| Recovery | Fail-closed transaction restores the module with recorded hash/owner/mode; recorded-kernel `depmod` and exact-path `modinfo` verified; reboot succeeds | `<commands/output>` | `<time>` | `<location>` | [ ] |
| Data recovery | Module, pool, same dataset, Ready testimony, cleared alarm, container, and original row all return | `<commands/output>` | `<time>` | `<location>` | [ ] |
| Runbook and evidence | Transcript, commands, outputs, timings, diagnosis, recovery, and teardown inventory are complete | `<inventory>` | `<time>` | `<location>` | [ ] |

Record the local `scripts/real-host-acceptance.sh --self-test` transcript with
the candidate. Every reboot, API, container, and testimony wait must show an
absolute deadline with locally bounded SSH attempts; a remote timeout alone is
not sufficient evidence of a bounded wait.

## #391 Core/CLI checklist claims

This matrix maps the direct Core/CLI real-host claims relevant to the combined
gate. It does not substitute for the separate 27-claim hosted-product record in
[`beta-acceptance.md`](beta-acceptance.md).

| #391 claim | Required proof | Evidence location | Result |
| --- | --- | --- | --- |
| Public installer uses one exact sealed candidate | immutable tag/SHA, assets, channel, installed equality | `<location>` | [ ] |
| amd64 Rocky core joins | public install and machine operation transcript | `<location>` | [ ] |
| arm64 Ubuntu edge joins | public install and machine operation transcript | `<location>` | [ ] |
| Host firewalls open exactly Ployz ports | provider + firewalld + UFW evidence | `<location>` | [ ] |
| Dockerfile builds native amd64 and arm64 | exact-commit operation receipt and stable index | `<location>` | [ ] |
| Railpack builds native amd64 and arm64 | exact-commit operation receipt and stable index | `<location>` | [ ] |
| Replicas land on both machines | machine/container testimony | `<location>` | [ ] |
| Managed HTTPS returns 200 | DNS, TLS, response, timing | `<location>` | [ ] |
| Cross-machine routing works through both gateways | local-replica-stop probes | `<location>` | [ ] |
| Route survives control-daemon restart | uninterrupted probe transcript | `<location>` | [ ] |
| Provisioned Volume is a real ZFS dataset | pin/list/mount/quota/dataset agreement | `<location>` | [ ] |
| Database row survives a real Rocky reboot | before/after row and same-dataset proof | `<location>` | [ ] |
| Pool import ordering is correct | systemd boot ordering and no manual import | `<location>` | [ ] |
| ZFS-module failure is loud and non-destructive | typed testimony, alarm, stopped workload, preserved state | `<location>` | [ ] |
| Control plane survives storage unavailability | service health and bounded command proof | `<location>` | [ ] |
| Recovery restores the original data | same dataset and row after restoration | `<location>` | [ ] |
| CLI smoke path | passing `scripts/cli-smoke-test.sh` command, output, and timing | `<location>` | [ ] |

## Phase transcript index

| Phase | Start/end UTC | Commands | Exit/status | Primary output | Evidence location |
| --- | --- | --- | --- | --- | --- |
| candidate preflight | `<times>` | `<commands>` | `<status>` | `<summary>` | `<location>` |
| host preflight/install/join | `<times>` | `<commands>` | `<status>` | `<summary>` | `<location>` |
| native builds/cancellation | `<times>` | `<commands>` | `<status>` | `<summary>` | `<location>` |
| deploy/network/restart | `<times>` | `<commands>` | `<status>` | `<summary>` | `<location>` |
| ZFS prepare/PostgreSQL write | `<times>` | `<commands>` | `<status>` | `<summary>` | `<location>` |
| normal reboot/persistence | `<times>` | `<commands>` | `<status>` | `<summary>` | `<location>` |
| module inventory/quarantine | `<times>` | `<commands>` | `<status>` | `<summary>` | `<location>` |
| broken-module reboot/assertions | `<times>` | `<commands>` | `<status>` | `<summary>` | `<location>` |
| restore/reboot/data recovery | `<times>` | `<commands>` | `<status>` | `<summary>` | `<location>` |
| teardown | `<times>` | `<commands>` | `<status>` | `<summary>` | `<location>` |

## Failure diagnosis and emergency recovery

- Failure phase and first failing assertion: `<phase/assertion>`
- Last known healthy state: `<evidence>`
- SSH available: `<yes/no; evidence>`
- Provider console evidence: `<location>`
- Rescue boot used: `<yes/no; provider event/location>`
- Boot/systemd diagnosis: `<commands/output/location>`
- Network/SSH diagnosis: `<commands/output/location>`
- ZFS module/pool/dataset diagnosis: `<commands/output/location>`
- Docker mount/container diagnosis: `<commands/output/location>`
- Ployz control/machine-role diagnosis: `<commands/output/location>`
- Recovery commands came exactly from `recovery.txt`: `<yes/evidence>`
- No destructive cleanup occurred before evidence and row recovery:
  `<yes/evidence>`
- Original row recovered: `<yes; digest/output/location>`

## Teardown and retention

| Resource | Identifier | Deleted/revoked UTC | Evidence |
| --- | --- | --- | --- |
| Rocky core and attached disks/snapshots | `<ids>` | `<time>` | `<location>` |
| Ubuntu edge and attached disks/snapshots | `<ids>` | `<time>` | `<location>` |
| Provider firewall/rules | `<ids>` | `<time>` | `<location>` |
| Rescue settings/placement resources | `<ids>` | `<time>` | `<location>` |
| DNS/certificate/lease resources | `<ids>` | `<time>` | `<location>` |
| Ephemeral provider SSH key | `<id/fingerprint>` | `<time>` | `<location>` |
| Local `ssh-agent` key | `<fingerprint>` | `<time>` | `<location>` |
| Git fixture/trust/credential resources | `<ids>` | `<time>` | `<location>` |
| Hosted fixture branch/resources | `<ids>` | `<time>` | `<location>` |

- Evidence retention owner and expiry: `<owner; UTC date>`
- Durable evidence location and access policy: `<location/policy>`
- Secret scan/redaction result: `<command/output/location>`
- Quarantine copies removed only after recovery/evidence review: `<evidence>`
- All #536 acceptance rows checked: `<yes>`
- All relevant #391 rows checked: `<yes>`
