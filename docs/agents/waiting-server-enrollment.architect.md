# Waiting Server enrollment: check in, wait, enroll in bulk

Status: architect package, not built. No code or PRs exist for it. The UI and CLI copy follow `dashboard/src/routes/_protected/cloud/$organizationSlug/_org/~/servers/-components/waiting-servers.prototype.html`.

In short. The user mints a Fleet Token with a two-word name and an expiry. Each host runs one line that installs the Ployz CLI, writes the token, and starts a small `ployz server checkin` loop under systemd. The loop checks in at once, so the host shows in Cloud as a Waiting Server within seconds. Host preparation runs in the background while the host waits. It installs Docker and builds ZFS ahead of time, without installing ZFS. Each check-in reports how far preparation has got. When the user enrolls the host, or the token enrolls on arrival, Cloud waits until the host is ready. The next check-in then answers `enroll` with a per-attempt Cloud Enroll Token bound to the founder or joiner role. The loop runs today's `ployz server add` path in-process. Cloud picks the founder, joins the rest 8 at a time under the existing pairing lock, and owns each attempt with one Inngest run per Server Enrollment. The daemon does not change.

```
 user_data / shell        waiting host (no ployzd yet)                       Cloud
 -----------------        ----------------------------                       -----
 curl ployz.sh |          install CLI, write /etc/ployz/fleet-token
   PLOYZ_ENROLL_TOKEN=pflt_ sh   ployz server checkin --install
                            writes ployz-checkin.service + ployz-prepare.service
                            starts the loop, exits 0 (seconds, no Docker yet)
                          loop: ployzd install --probe --json -> HostReport
                                POST /api/checkin (Bearer pflt_, Ployz-Signature) --> upsert waiting_server
                                  report + prep{docker, zfs}                          facts, blockers, prep
                                <-- wait{retryAfter, probe?}                          status cell: Installing Docker
                          ployz-prepare.service (started by the loop, flock)                   / Building ZFS / Ready
                            1 install Docker
                            2 build ZFS: download or dkms build, no install
                                                                    user: Enroll  --> server_enrollment rows
                                                                    or on arrival     + one Inngest run each
                                                                    (ready only)      row reads Queued until
                                                                                      prep is ready
                                POST /api/checkin <-- enroll{attemptId, token,        founder released first,
                                                        name, storage, policy, role}  joiners after founded
                          server_add_in_process(instruction)    today's path
                            zfs: reuse the built module | none: discard the build
                                POST /api/enroll (role-bound token)              --> founder: initialize
                                                                                     joiner: not_yet{queued} until
                                                                                     a join slot frees, then join
                                POST /api/enroll/callback, /report               --> server/enrollment.completed
                          systemctl disable --now ployz-checkin; rm fleet-token
```

## Problem

Today one Server enrolls per pasted command. The first `/api/enroll` to insert `organization_pairing` wins the Founding Claim (`claimOrLoadEnrollment`, `dashboard/src/modules/machines/enrollment.server.ts`). The token lives 24 hours (`enrollmentExpiry`). A fleet hits four problems.

- 200 user_data scripts race for the Founding Claim.
- Nobody sees host facts before ZFS is attempted.
- Every refusal (`verify_docker`, `prepare_storage`) fires mid-install, after mutations have run.
- Every joiner reaches the founder's daemon at the same moment through `session.register` inside the `/api/enroll` handler.

There is no stop between "the box exists" and "the box is in a Cluster". Users can't pick the founder. They can't fix a 20 GB disk and retry without a new token.

Constraints traced in code.

- **Stable promise.** `ployz server add` with today's token keeps working. `/api/enroll` and `EnrollWire` (`core/crates/ployz/src/cloud_enroll.rs`) can only gain optional fields. `EnrollWire` already ignores unknown fields, except inside `pairing`.
- **Single claimant.** `machine_enrollment_token` (`dashboard/src/modules/machines/tables.ts`) already has single-claimant semantics through `claimedMachineId` and `claimedPublicKey`. A per-attempt token is a row in that table, not a new credential kind.
- **Identity.** The daemon generates `MachineId`, the WireGuard key and the Management Secret on the first `machine.json` load. A waiting host has no daemon, so it has no `MachineId`. The waiting identity must be separate.
- **Refusals.** Host refusals live in `ployzd/src/installer/{host,storage}.rs` as `pub(super)` checks interleaved with mutations. `SetupReport::read_host` is the only pure report. The code has no 30 GB check, no cgroup v2 detection, no empty-disk listing and no port probe.
- **Crate boundary.** `ployz` and `ployzd` don't depend on each other. The CLI downloads `ployzd` and spawns it as a one-shot installer (`ployz/src/provisioning.rs`). That installer process is where host probing lives without a daemon.
- **Install script.** `core/install.sh` installs only the CLI binary. It takes no token and installs no Docker. `.github/workflows/ployz-sh.yml` publishes it to `ployz.sh`.
- **Inngest rows.** Rows that own pending or running Inngest state go through `attemptLifecycle` (`dashboard/src/modules/inngest/attempt-lifecycle.ts`), as `serverDrainAttempt` does.

## Usage

### Add servers

The user opens Add servers. Cloud fills in a name such as `swift-heron`. The user can change it before creating. Expiry defaults to 1 day, with 7 days and 30 days on offer. One switch, Enroll on arrival, reveals one field, Up to N servers. Create shows the token once with three tabs.

```sh
curl -fsSL https://ployz.sh | sudo PLOYZ_ENROLL_TOKEN=pflt_… sh
```

```yaml
#cloud-config
runcmd:
  - curl -fsSL https://ployz.sh | PLOYZ_ENROLL_TOKEN=pflt_… sh
```

```hcl
resource "hcloud_server" "web" {
  count     = 200
  user_data = <<-EOT
    #cloud-config
    runcmd:
      - curl -fsSL https://ployz.sh | PLOYZ_ENROLL_TOKEN=${var.ployz_enroll_token} sh
  EOT
}
```

The hint under the script says "Installs Docker and Ployz. Nothing joins until you say so." With Enroll on arrival on, it says "Installs Docker and Ployz, then joins. Stops after N." Below it, "Shown once. Expires in 24h." The dialog then listens for servers and shows them as they arrive.

Each host shows under Waiting within seconds of its install line finishing. Its status cell reads "Installing Docker", then "Building ZFS" where the host builds its own ZFS module, then "Ready". The user can select rows and click Enroll at any point. A row that isn't ready yet reads "Queued" until its preparation finishes, then starts. Every host prepares and installs in parallel. The first server joins, then the rest join 8 at a time.

A blocked host doesn't fail cloud-init. `checkin --install` exits 0 once the loop runs, before Docker is installed, and Cloud shows why the host can't join yet.

### Operator CLI

```sh
ployz server token create                         # Created amber-falcon · expires in 24h
ployz server token create --name asg --auto --max 50 --expires 7d
ployz server token ls                             # NAME  JOINS  SERVERS  EXPIRES
ployz server token revoke amber-falcon            # 182 waiting servers removed. Servers already in stay.

ployz server ls --waiting                         # NAME  CPU  MEMORY  DISK  STATUS
ployz server show hz-fsn1-001                     # machine, disks, storage, network, from IP · provider · token
ployz server reject 'ip-10-0-9-*'                 # confirms; "They won't come back."

ployz server enroll hz-fsn1-001                   # empty Organization: "Your first server. Everything else joins it."
ployz server enroll 'hz-fsn1-*' --name 'web-{n}'  # lists the ones that can't join yet, confirms, shows progress
ployz server enroll --all                         # breaks down by provider, flags Unknown, confirms
ployz server enroll --status
```

`--expires` takes `1d`, `7d` or `30d` and defaults to `1d`. `--auto` requires `--max`. Creating an auto token prints "Anyone with this token can add servers. Keep it secret."

A glob or `--all` prints the matched rows with their source IPs before it acts. `reject` and a multi-row `enroll` confirm with y/N, and `--yes` skips the prompt. With one selected server on an empty Organization, that server is the first server. With more than one, `--first <name>` picks it.

`server enroll` takes the same volume choices as the Enroll sheet. `--storage zfs` uses a spare disk when the host has one and falls back to Docker only where ZFS can't run. `--no-spare-disk` keeps ZFS on the main disk. `--no-fallback` skips hosts that can't run ZFS. `--storage none` is Docker only. A skipped row prints its reason.

### On the host

```sh
ployz status                      # Installing Docker / Building ZFS / Waiting for approval / Queued / In line · 8 servers ahead / Live as web-1 in Acme
journalctl -fu ployz-checkin      # Approved as web-1 by ada@acme.dev … Removed the token. Done here.
```

`journalctl -u ployz-prepare` shows the preparation log. A blocked host prints the fix, for example "Disk is 20 GB. Ployz needs 30 GB." and "Grow the disk and it clears on its own." A failed Docker install is a blocker with its own fix line. A failed ZFS build is not a blocker. The host can still join with Docker volumes, and the row says ZFS isn't available. A failed attempt logs the failure and goes back to waiting. A rejected host logs "Rejected by Acme" and stops. An expired token logs "Token expired. Run the install again with a new token." and stops.

### One server stays easy

- `curl … | sh && sudo ployz server add` with today's token, or from a signed-in CLI, enrolls at once and doesn't change.
- In the UI, a lone Waiting Server in an Organization with no Servers opens the Enroll sheet by itself.

### Cloud server functions

```ts
listWaitingServers({ organizationSlug }): WaitingServerView[]
planServerEnrollment(input, rows, cluster): EnrollmentPlan            // pure, previewable
startServerEnrollment({ organizationSlug, plan }): { started: number }
cancelServerEnrollment({ organizationSlug, attemptIds }): void
rejectWaitingServers({ organizationSlug, ids }): { rejected: number }
createFleetToken({ organizationSlug, name, expiresIn: "1d" | "7d" | "30d", autoEnroll: { maxServers: number } | null }): { token: string; id: FleetTokenId; name: string }
listFleetTokens({ organizationSlug }): FleetTokenView[]
revokeFleetToken({ organizationSlug, id }): { waitingRemoved: number }
```

Enroll, reject and revoke delete no Server. The dashboard and agents don't ask before any of them. The CLI asks before reject and multi-row enroll, because a glob can match more than the user meant.

## Shape

### Data: two new owners, one additive token change

```ts
// dashboard/src/modules/servers/waiting/tables.ts
export const fleetToken = pgTable("fleet_token", {
  id: uuid("id").defaultRandom().primaryKey(),
  organizationId: uuid("organization_id").notNull().references(() => organization.id, { onDelete: "cascade" }),
  name: text("name").notNull(),                             // two words, generated; editable before create
  tokenHash: text("token_hash").notNull(),                  // sha256 of pflt_…, shown once
  expiresAt: timestamp("expires_at", { withTimezone: true }).notNull(),   // now + 1d | 7d | 30d
  autoEnrollMax: integer("auto_enroll_max"),                // null: review each; n > 0: enroll on arrival, up to n
  createdByUserId: uuid("created_by_user_id").notNull(),
  lastUsedAt, revokedAt, createdAt,
}, (t) => [
  uniqueIndex("fleet_token_hash_idx").on(t.tokenHash),
  uniqueIndex("fleet_token_name_idx").on(t.organizationId, t.name).where(sql`revoked_at is null`),
]);

/** Sole writer: POST /api/checkin. Facts are the host's own observation. */
export const waitingServer = pgTable("waiting_server", {
  id: uuid("id").defaultRandom().primaryKey(),
  organizationId: uuid("organization_id").notNull().references(() => organization.id, { onDelete: "cascade" }),
  fleetTokenId: uuid("fleet_token_id").notNull().references(() => fleetToken.id, { onDelete: "cascade" }),
  checkinKey: text("checkin_key").notNull(),                // ed25519 public key, hex: the waiting identity
  report: jsonb("report").notNull().$type<HostReport>(),    // decoded with a 64 KiB cap
  observedIp: text("observed_ip").notNull(),                // request source address, not a host claim
  probe: jsonb("probe").$type<ProbeResults | null>(),
  firstSeenAt, lastCheckInAt,
  rejectedAt: timestamp("rejected_at", { withTimezone: true }),   // tombstone: same key gets stop{rejected}
}, (t) => [uniqueIndex("waiting_server_key_idx").on(t.organizationId, t.checkinKey)]);

/** Sole writer: the server-enrollment Inngest run, through attemptLifecycle. */
export const serverEnrollment = pgTable("server_enrollment", {
  id: uuid("id").primaryKey(),                              // attemptId, sent in the instruction
  organizationId: uuid("organization_id").notNull(),
  waitingServerId: uuid("waiting_server_id").notNull().references(() => waitingServer.id, { onDelete: "cascade" }),
  fleetTokenId: uuid("fleet_token_id").notNull(),           // counts toward autoEnrollMax
  role: text("role").notNull().$type<"founder" | "joiner">(),
  instruction: jsonb("instruction").notNull().$type<EnrollInstructionBody>(),   // name, storage, policy
  enrollTokenId: uuid("enroll_token_id").notNull(),         // row in machine_enrollment_token
  encryptedToken: text("encrypted_token").notNull(),        // SecretEncryption; dropped on end
  state: text("state").notNull().$type<EnrollmentState>(),
  stage: text("stage").$type<EnrollmentStage | null>(),
  machineId: text("machine_id").$type<MachineId>(),         // copied from the token's claimedMachineId
  failure: jsonb("failure").$type<{ stage: EnrollmentStage; message: string } | null>(),
  inngestRunId: text("inngest_run_id"),
  requestedByUserId: uuid("requested_by_user_id"),          // null when the token enrolled it on arrival
  requestedAt, endedAt,
}, (t) => [
  uniqueIndex("server_enrollment_one_open_idx").on(t.waitingServerId).where(sql`ended_at is null`),
  uniqueIndex("server_enrollment_one_founder_idx").on(t.organizationId).where(sql`ended_at is null and role = 'founder'`),
]);

// machine_enrollment_token gains two nullable columns (additive):
//   enrollmentRole: "founder" | "joiner" | null   -- null keeps today's race
//   serverEnrollmentId: uuid | null
```

A token has two knobs and no rules. `expiresAt` bounds when it works. `autoEnrollMax` bounds how many servers it can enroll on its own. It carries no provider, source-IP or hardware rule. Hardware fit, such as the 30 GB disk floor, is a property of the host and is checked by `assess` at enroll time for every path.

The token's used count is derived. It counts the token's `serverEnrollment` rows that are open or ended `joined`. A failed or cancelled attempt frees its count. The Tokens page shows "N waiting · M joined" for a reviewed token and "joined/max" for an auto token.

No status column is synced between tables. The list a user sees is derived at read time. It is `waitingServer` rows without `rejectedAt`, left-joined with their latest `serverEnrollment`. The check-in route writes facts and the run writes attempt state, so no row has two writers.

### State machine

```
              check-in (live fleet token + checkin key signature)
                       |
                       v
   +------------> WAITING ------------ reject ------------> REJECTED  (tombstone; host gets stop{rejected}, unit stops)
   |  blocked is     |   \
   |  a status       |    \ token revoked or expired, no open attempt
   |                 |     +------------------------------> REMOVED   (row deleted; host gets stop{token_*})
   |  enroll /       |    \ silent 7 days, no open attempt (sweep)
   |  on arrival     v     +------------------------------> REMOVED
   |        ENROLLING{ awaiting_founder | instructed | running(stage) }
   |                 |                         |
   |  failed /       |                         | completed event
   |  cancelled      |                         v
   +-----------------+                       JOINED   (stays in the Enrolling list; swept after 7 days)
```

```ts
type EnrollmentState =
  | "awaiting_founder"   // joiner while the Organization has no founder; instruction withheld
  | "instructed"         // released; host has not picked it up
  | "running"            // host reported it started
  | "joined" | "failed" | "cancelled";        // terminal; failed and cancelled return the host to WAITING

type EnrollmentStage = "installing" | "storage" | "enrolling" | "queued" | "joining" | "publishing";
```

`blocked` isn't a state. It is WAITING with a non-empty `report.blockers`. It clears when the next report comes back clean. Recheck needs no button, because every poll is a recheck. `Offline` shows when `lastCheckInAt` is older than three intervals.

Preparation isn't a state either. It is the latest check-in's `prep` field, read at display time. A host is ready when `prep.docker` is `ready` and the report has no blockers. It is ready for ZFS when `prep.zfs` is also `ready`. An attempt that is `instructed` on a host that isn't ready for its storage displays as "Queued", and Cloud keeps answering `wait`. No column records it, so nothing has to be synced when preparation finishes.

At enroll, `installing` installs `ployzd` and its unit. Docker is already there. `storage` installs ZFS from the module preparation built, or throws the build away when the chosen storage is `none`, then creates the pool.

### UI contract

The Servers page shows a Waiting section and an Enrolling section. Both hold still while rows change. These rules are invariants, and the verify run checks them.

- **Fixed columns.** Waiting uses one fixed grid (checkbox, name and OS, source IP and provider, size, disks, volumes, status). Enrolling uses another (dot, name, stage, steps, detail, action). No column resizes with content.
- **Fixed counters.** Enrolling shows six counters in a fixed order. Each always renders, at zero too.

  | Counter | States and stages |
  |---|---|
  | Starting | `awaiting_founder`, `instructed`, including attempts shown as "Queued" |
  | Installing | `installing` |
  | Setting up ZFS | `storage` |
  | Enrolling | `enrolling` |
  | In line | `queued` |
  | Joining | `joining`, `publishing`, shown as "N / 8" |

- **Insertion order.** New rows append at the bottom. Nothing prepends.
- **Joined rows stay.** A joined row stays in the Enrolling list, labelled Joined, until the user leaves the page or the sweep removes it.
- **Sent back goes last.** A failed or cancelled attempt returns its host to the bottom of Waiting, with "Failed at <stage>".
- **Ten rows tall.** Each list scrolls inside a box that holds 10 rows.
- **Preparing rows select.** A Waiting row's status cell reads "Installing Docker", "Building ZFS" or "Ready". A preparing row can be checked and enrolled. Blocked and Offline rows show why and can't be checked.
- **Preparation reuses the cells.** Progress shows in the status cell's bold line, where "Ready" sits today, with the last-seen time below it. A failed Docker install shows the way a blocker does, as "Docker install failed" with its fix below. A failed ZFS build shows the way a warning does, as "ZFS build failed" in the second line of a selectable row, and the volumes cell offers Docker only. No column or row is added, and the two-line cell doesn't change height when the text changes.
- **Queued in place.** An enrolled row whose host isn't ready reads "Queued" in the Enrolling stage cell. It moves on to Installing in the same row when preparation finishes.

Copy is two words where it can be. "First one, then 8 at a time." "N rejected. They won't come back." "Expired or revoked tokens stop new servers. Ones already in stay."

### Placement

| Piece | Lives in | Why |
|---|---|---|
| Host facts, blockers | `ployzd` installer, `installer/assess.rs`, run as `ployzd install --probe --json` | The refusals already live there. It is a one-shot process, not the daemon. |
| Host preparation (Docker install, ZFS build) | `ployzd install --prepare`, run as `ployz-prepare.service`, which the loop starts | Same installer and the same install functions. A systemd unit outlives a loop restart and runs once at a time. |
| Install line | `core/install.sh` reading `PLOYZ_ENROLL_TOKEN` | The script already ships on `ployz.sh`. |
| Check-in loop, systemd unit, in-process enroll, host status | CLI, `ployz/src/checkin/` | It ships everywhere already and reuses `server add`. |
| Fleet Token, Waiting Server, Server Enrollment | Cloud Postgres | Bookkeeping about non-members. |
| Founder choice, naming, storage resolution | `planServerEnrollment`, pure | Cloud acts as the operator (bet 6). |
| Attempt drive | Inngest `server-enrollment`, one run per row | One run per resource. |
| Join gate | `enrollMachine`, under the existing `organization_pairing` for-update lock | A derived count with no new serialization. |
| Membership | Config Store, through today's `/api/enroll` → `session.register` → join | Unchanged. Waiting servers never appear in the Config Store. |
| Daemon | No change | Bet 10. |

### Host side (Rust)

```rust
// core/crates/ployzd/src/installer/assess.rs (new): the single source of every refusal
pub struct HostFacts {
    pub os: OsRelease, pub kernel: String, pub arch: String,
    pub virt: Virtualization,              // Vm | BareMetal | OpenVz | Lxc { privileged }
    pub secure_boot: Option<bool>,
    pub cgroup: CgroupVersion,             // V1 | V2 | Hybrid
    pub cpu_count: u32, pub memory_total_bytes: u64,
    pub disks: Vec<Disk>,                  // lsblk: root + whole disks with no partitions, signature, mount or holders
    pub docker: DockerFacts,               // Absent | Present { version, api, buildx, containerd_store }
    pub zpool_present: bool,
}
pub struct Disk { pub path: String, pub by_id: Option<String>, pub size_bytes: u64, pub role: DiskRole } // Root { available } | Empty

pub fn read_facts() -> Result<HostFacts, Error>;   // IO, no mutation
pub fn assess(facts: &HostFacts) -> Assessment;     // pure

pub struct Assessment {
    pub blockers: Vec<Blocker>,            // disk < 30 GiB, incompatible Docker, no systemd
    pub warnings: Vec<Blocker>,            // cgroup v1
    pub zfs: ZfsEligibility,               // Eligible { targets: MainDisk | Disk{by_id}, build_minutes } | Ineligible { reason }
}
/// Stable code + English message. Codes are open: Cloud shows the message for codes it doesn't know.
pub struct Blocker { pub code: &'static str, pub message: String }
```

The installer refuses on `assess` for the requested storage, and check-in reports the whole value. Both run the same function, so the list and the refusal can't drift.

```rust
// core/crates/ployz/src/checkin/mod.rs (new)
pub async fn run(args: CheckinArgs) -> Result<CheckinEnd, Error>;      // `ployz server checkin`
pub enum CheckinEnd { Enrolled(MachineId), Stopped(StopReason) }
pub enum StopReason { Rejected, TokenExpired, TokenRevoked }

async fn pass(state: &mut LocalCheckin, cloud: &CheckinClient) -> Result<Next, Error>;  // probe, post, act
enum Next { Sleep(Duration), Probe(ProbeRequest), Enroll(Box<Instruction>), Stop(CheckinEnd) }

struct LocalCheckin { key: CheckinKey, attempt: Option<AttemptRecord>, last: HostStatus }
//   /var/lib/ployz/checkin/{key,attempt.json,status.json}
async fn enroll(instruction: Instruction) -> Result<MachineId, StageFailure>;  // today's server add body, in-process
```

`checkin --install` writes `ployz-checkin.service` and `ployz-prepare.service`, starts the loop and exits 0. It doesn't install Docker and doesn't wait for anything. It reuses an existing key file. A rerun of the install line on the same host keeps its identity, so a rejected host stays rejected.

`enroll` calls the same function `ployz server add` calls after it resolves the token. The per-attempt token never touches argv or disk. A loop restarted mid-install resumes through the existing local enrollment record, because Cloud keeps serving the same instruction while the attempt is open.

The loop starts `ployz-prepare.service` on its first pass and reads `/var/lib/ployz/prepare/state.json` on every pass to fill `prep`. The loop writes `status.json` after every pass. `ployz status` reads it on a host and leads with a This server block. On success the loop disables its unit, deletes `/etc/ployz/fleet-token` and exits. On failure it reports the failure on the next check-in and keeps polling. On `stop` it logs the reason, disables its unit and deletes the token file.

### Host preparation

Preparation is one background job per host. The loop owns starting it. `ployz-prepare.service` runs `ployzd install --prepare` as a oneshot unit.

Preparation is on by default. It always installs Docker, because every enroll needs it. The one opt-out is the ZFS build, for power users who know they won't use ZFS: `PLOYZ_SKIP_ZFS=1` on the install line passes `--skip-zfs` through `checkin --install` to `ployzd install --prepare --skip-zfs`. The install line copy in the dashboard never shows it. An opted-out host reports `zfs: "off"`, which counts as ready. ZFS eligibility in `report.zfs` is unchanged, so an enroll with storage `zfs` still works and builds ZFS at enroll time, as today.

```rust
// core/crates/ployzd/src/installer/prepare.rs (new)
pub fn prepare(paths: &InstallPaths) -> Result<(), Error>;   // `ployzd install --prepare`
//   flock /var/lib/ployz/prepare/lock (non-blocking; held: exit 0, the other run owns it)
//   step docker: assess for storage none, refuse on a blocker, install Docker through today's path
//   step zfs:    assess ZFS eligibility; ineligible -> skipped; else build ahead (below)
//   writes /var/lib/ployz/prepare/state.json after every step change

pub struct PrepState {
    pub docker: StepState,                 // Installing | Ready | Failed { message }
    pub zfs: ZfsStep,                      // Installing | Ready | Failed { message } | Skipped { reason }
    pub added_packages: Vec<String>,       // build deps this job installed; nothing else is removed later
    pub zfs_build: Option<ZfsBuild>,       // Ubuntu { downloaded } | Dkms { module, version, source_dir }
}
pub fn discard_zfs_build(state: &PrepState) -> Result<(), Error>;  // enroll with storage none
```

Rules.

- **One at a time.** `systemctl start` on a running oneshot unit is a no-op, and the flock backs it up. A loop that restarts mid-prep never starts a second job. A restarted loop finds the unit running or finished and reads the state file.
- **Idempotent.** Each step checks its end state first. Docker active, the module already built for the running kernel, and the download already cached each skip the step. A job killed mid-step reruns it from the start. apt and dnf recover their own interrupted state, as today's installer relies on.
- **Sequential.** Docker first, then ZFS. Both steps take the apt or dnf lock, so they never overlap.
- **Retry.** A failed step stays failed until the loop restarts the unit, 10 minutes later, at most three times. A rerun of the install line also restarts it.
- **Never storage.** Preparation never creates a pool, never touches a disk, and never writes `machine.json`. Pool creation stays at enroll, on the chosen device (C10).

**ZFS builds ahead, and installs nothing.** The routes are today's `ZfsRoute` in `core/crates/ployzd/src/installer/storage.rs`.

| Route | Preparation does | Preparation leaves |
|---|---|---|
| Ubuntu | Runs `apt-get install --download-only zfsutils-linux`. The module ships prebuilt in Ubuntu's kernel packages, so there is nothing to build. | `.deb` files in the apt cache. |
| Debian 12 and 13 | Adds the `contrib` source as `prepare_debian_zfs` does. Installs only the build deps, `linux-headers-<kernel>`, the flavour's headers meta package and `dkms`. Fetches `zfs-dkms` with `apt-get download`, unpacks its source into `/var/lib/ployz/prepare/zfs/`, then runs `dkms add` and `dkms build`. It never runs `dkms install`. | A built module in the DKMS tree, the source, the build deps. |
| Amazon Linux 2023 | Runs the tarball download, the checksum, `kernel-devel` plus `OPENZFS_BUILD_DEPENDENCIES`, configure and `make rpm-utils rpm-dkms` into `/var/lib/ployz/prepare/zfs/`, as `prepare_amazon_zfs` does. Then unpacks the built `zfs-dkms` RPM's source with `rpm2cpio` into the same cache and runs `dkms add` and `dkms build` on it. It doesn't install the RPMs. | The RPMs, a built module in the DKMS tree, the build deps. |

Nothing goes into `/lib/modules`. Nothing is loaded. No ZFS package is installed. `modules-load.d` isn't written. The Secure Boot, OpenVZ and LXC refusals in `prepare_zfs` run first, and a refused host reports `zfs: skipped` with the reason.

**At enroll.**

- Storage `zfs`. Today's `prepare_zfs` runs. On Debian, apt installs `zfs-dkms` and `zfsutils-linux`. On Amazon Linux, dnf installs the cached RPMs. The install should reuse the module DKMS already built for the running kernel. Then `modprobe`, the ARC cap, the smoke pool and the real pool run as today.
- Storage `none`. `discard_zfs_build` runs `dkms remove` for the built module, deletes `/var/lib/ployz/prepare/zfs/`, removes the `contrib` source the job added, and removes only the packages in `added_packages`. Packages the host had before preparation stay. On Ubuntu it runs `apt-get clean` for the downloaded files.

**UNVERIFIED.** Whether apt installing `zfs-dkms`, or dnf installing the `zfs-dkms` RPM, reuses a module DKMS already built for the running kernel, instead of compiling it again. The package's postinst runs `dkms add`, `dkms build` and `dkms install`. DKMS skips `build` for a module and version it already built, but that isn't proven for these two packages. If it doesn't reuse it, the fallback is that enroll runs `dkms install` on the prebuilt module itself, then installs `zfsutils-linux` or the userland RPMs without `zfs-dkms`. On Amazon Linux the userland RPMs may require the `zfs-kmod` capability that `zfs-dkms` provides, so the fallback may still install `zfs-dkms` and accept the rebuild there. A Boat lane in C7 settles this.

### Wire protocol (new, additive)

**Server → Cloud.** `POST /api/checkin` takes three headers.

- `Authorization: Bearer pflt_…`
- `Ployz-Timestamp`
- `Ployz-Signature: v1=<b64url(ed25519(ctx || checkinKey || 0 || ts_be || sha256(body)))>`, with ctx `ployz-checkin-v1\0`

This copies the observe push framing in `ployzd/src/observe/ship.rs` with its own context.

```ts
type CheckinRequest = {
  protocolVersion: 1;
  checkinKey: string;                // must match the signature
  cliVersion: string;
  report: HostReport;
  attempt?: { attemptId: string; stage: EnrollmentStage; failure?: { stage: EnrollmentStage; message: string } };
  probe?: { nonce: string; heardFrom: string[]; bindFailed: string[] };
  prep?: {                           // optional, so /api/checkin stays frozen; absent reads as ready
    docker: "installing" | "ready" | "failed";
    zfs: "installing" | "ready" | "failed" | "skipped" | "off";
  };
};
type HostReport = {
  hostname: string; publicIp?: string; provider?: string;
  os: { id: string; version: string }; kernel: string; arch: string; virt: string;
  cpuCount: number; memoryTotalBytes: number;
  disks: { path: string; byId?: string; sizeBytes: number; role: "root" | "empty"; availableBytes?: number }[];
  docker: { kind: "absent" } | { kind: "present"; version: string; api: string; compatible: boolean };
  cgroup: "v1" | "v2" | "hybrid";
  zfs: { kind: "eligible"; targets: ({ kind: "main_disk" } | { kind: "disk"; byId: string })[]; buildMinutes?: number }
     | { kind: "ineligible"; code: string; message: string };
  blockers: { code: string; message: string }[];
  warnings: { code: string; message: string }[];
};
```

**Cloud → server.** The answer is one closed verb. The host reads an unknown `kind` as `wait{60}`.

```ts
type CheckinResponse =
  | { kind: "wait"; retryAfter: number; probe?: ProbeRequest }
  | { kind: "enroll"; attemptId: string; token: string;           // per-attempt cet_, role-bound
      name: string; role: "founder" | "joiner";
      storage: { kind: "none" } | { kind: "zfs" } | { kind: "zfs"; device: string };
      policy: { labels: Record<string, string>; acceptsBuilds: boolean; acceptsServices: boolean; acceptsIngress: boolean } }
  | { kind: "stop"; reason: "rejected" | "token_expired" | "token_revoked"; message: string };

type ProbeRequest = {                 // bounded window; nothing listens after it
  nonce: string; windowMs: number;    // <= 10_000, host clamps
  listen: ("udp:51820" | "tcp:80" | "tcp:443")[];
  send: { target: string; port: 51820 }[];   // other selected waiting hosts' observed IPs
};
```

An unknown token hash answers 401. The host treats 401 like `stop{token_revoked}`.

`prep` carries progress only. The reasons travel in the existing lists. A failed Docker install adds a blocker such as `docker_install_failed` with its message. A failed or skipped ZFS build adds a warning, and `report.zfs` reads `ineligible` with that code. So the planner falls back to Docker volumes, or skips the row under `--no-fallback`, with no new rule. A host that sends no `prep`, such as a future CLI that prepares differently, reads as ready.

**`/api/enroll`.** Every addition is optional.

- `not_yet` gains `reason?: "founder" | "queued"` and `ahead?: number`. Today's CLI ignores both. The new CLI waits past `FOUNDER_WAIT` while the reason is `queued`, because that is Cloud's patience, not a safety bound. `ahead` feeds "In line · 8 servers ahead".
- `StorageChoice` gains `zfs` with an optional `device`.
- `EnrollIdentity` doesn't change. The token carries the role and the attempt.

The installer gains `--storage zfs:<device>`.

### Cloud side

`checkIn` (`modules/servers/waiting/checkin.server.ts`) is the only writer of `waitingServer`. It runs one transaction.

1. Load the `fleetToken` by hash. Verify the signature against `checkinKey` and check timestamp skew of 5 minutes or less. Apply per-token and per-source-IP rate limits.
2. If the token is revoked or past `expiresAt`, look for an open attempt on this key. With one, keep going so the attempt can finish. Without one, delete the row and answer `stop{token_revoked | token_expired}`.
3. Upsert on `(organizationId, checkinKey)`. A tombstoned key gets `stop{rejected}`. A new key over the per-token cap of 1000 waiting rows gets 429.
4. If `attempt` is present, emit `server/enrollment.reported`. The run applies it.
5. Read the open `serverEnrollment`.
   - If it is `running`, answer `enroll` with the decrypted token.
   - If it is `instructed`, answer `enroll` only when the host is ready for the attempt's storage. That means `prep.docker` is `ready`, and `prep.zfs` is `ready` when the storage is `zfs`. Otherwise answer `wait{10}`, and the row reads "Queued".
   - Otherwise, if the token has `autoEnrollMax`, the Organization already has a Cloud Pairing, the host is ready with no blockers, and the used count is under the max, call `startServerEnrollment` with the default plan and answer `wait{2}`. A host still preparing doesn't start an attempt and doesn't count.
   - Otherwise answer `wait{10, probe?}`.

The default plan for enroll on arrival uses the hostname as the name, ZFS on a spare disk when there is one, Docker only where ZFS can't run, and today's default policy. It runs only once the host is ready, so the ZFS choice reads a finished build. A host whose ZFS build failed gets Docker volumes.

`planServerEnrollment` is pure. Its input is the Enroll sheet.

```ts
type EnrollInput = {
  ids: WaitingServerId[];
  first?: WaitingServerId;                      // only when the Organization has no Cloud Pairing
  storage: { kind: "zfs"; useSpareDisk: boolean; fallbackToDocker: boolean } | { kind: "none" };
  names: { kind: "hostnames" } | { kind: "pattern"; pattern: string };   // "web-{n}"
  runs: { services: boolean; ingress: boolean; builds: boolean };
};
```

It resolves storage per row, applies names, and skips rows with blockers or ineligible storage, giving the reason. A row still preparing isn't skipped. A row whose ZFS build is still running resolves `zfs` as eligible, and if the build later fails the attempt ends `failed` with "ZFS build failed" and the host goes back to Waiting. It picks the founder only when the Organization has no Cloud Pairing. That is the explicit pick, or else the only selected row. With more than one row and no pick it refuses. It also returns the spread, the count of distinct providers and /16 networks, which the sheet shows as "N networks, M providers. Make sure they're all yours." when it passes one provider or three networks.

`startServerEnrollment` inserts `serverEnrollment` rows plus per-attempt `machine_enrollment_token` rows, then sends one `server/enrollment.requested` event per row.

`revokeFleetToken` sets `revokedAt` and deletes the token's waiting rows that have no open attempt, in one transaction. It returns the count for "N waiting removed". Open attempts finish. The sweep deletes an expired token's waiting rows the same way.

`rejectWaitingServers` sets `rejectedAt` and cancels any open attempt on those rows.

**Founding Claim.** The start transaction can't insert `organization_pairing` itself, because that row needs the founder's `MachineId` and public key. Those exist only once its daemon loads `machine.json`. So Cloud assigns the founder through the token. Exactly one per-attempt token carries `enrollmentRole = "founder"`, and `claimOrLoadEnrollment` changes in one place.

```ts
if (token.enrollmentRole === "joiner") skipFoundingInsert();   // a joiner can never found
// founder and legacy (null) tokens try the insert as today
```

The partial index `server_enrollment_one_founder_idx` refuses a second concurrent founder plan.

A legacy `server add` racing a designated founder can still win the insert. The designated founder then gets `not_yet{reason:"founder"}` and becomes a joiner once the legacy founder completes. That is today's legacy semantics, accepted under bet 6.

```ts
// modules/servers/waiting/enrollment-run.server.ts
export const serverEnrollmentRun = attemptLifecycle({
  run: {
    id: "server-enrollment",
    triggers: [{ event: "server/enrollment.requested" }],
    cancelOn: [{ event: "server/enrollment.cancel", match: "data.attemptId" }],
    handler: async ({ event, step, runId }, runEffect) => {
      // 1 joiner + no founder: waitForEvent("organization/founded", match organizationId, 2h);
      //   timeout -> re-read; still unfounded -> end failed("First server didn't finish").
      // 2 release: UPDATE ... SET state='instructed' WHERE state='awaiting_founder'   (fenced)
      // 3 loop: waitForEvent(reported | completed, match attemptId, 30m)
      //   reported(stage) -> record; reported(failure) -> end failed; completed -> end joined
      //   timeout -> host still checking in: keep waiting (max 12 rounds); silent -> end failed
      // 4 joined + founder -> send organization/founded
    },
  },
  cancelId: "server-enrollment-cancel",
  closeRun: closeServerEnrollment,      // terminal state, endedAt, expire token, drop encryptedToken
  sweep: { id: "server-enrollment-sweep", closeStale: sweepWaitingAndEnrollments },
});
```

Only the release and the terminal writes are late writes. Each is a conditional update on the current state, so a retried step does nothing.

Ending an attempt sets its token's `expiresAt = now()`. A late host that picks up a stale instruction gets 401 from `/api/enroll` and reports that on its next check-in.

`completeMachineEnrollment` gains one line. When the token has `serverEnrollmentId`, it emits `server/enrollment.completed {attemptId, machineId}`. The existing founder events (`sync-cluster-domain`, `deploy-to-first-server`) stay where they are.

### Batching and founder

The first server joins alone. Everything else joins 8 at a time.

- Preparation runs on every host from the moment it checks in, before anyone enrolls it. It is local work and the slow part. ZFS builds on Debian and Amazon Linux take up to 20 minutes, and they now overlap the time the host spends waiting.
- Install at enroll runs in parallel on every host, and is short because Docker and the ZFS module are already there.
- Only the join step is gated. It calls `session.register` through the founder's daemon, and the joiner's Corrosion bootstraps from every peer it can see.
- **Gate.** In `enrollMachine`, for tokens with `serverEnrollmentId`, the check runs inside the existing for-update transaction on `organization_pairing`. It counts this Organization's open attempts with `stage = 'joining'`.
  - At `JOIN_SLOTS` (8) or above, it answers `not_yet{retryAfter: 5, reason: "queued", ahead}`.
  - Otherwise it marks this attempt `joining` and proceeds.
  - The attempt moves past `joining` when the callback completes.
- A failed or swept attempt frees its slot by ending. There is no counter to leak. Legacy tokens skip the gate.
- **Founder is ready first.** The founder's instruction is held like any other until its host is ready. Joiners stay `awaiting_founder` until it joins, so no unready host ever founds and no joiner outruns it.
- **Founder failure.** Joiners stay `awaiting_founder`. After the 2-hour wait their runs end `failed` and they return to the bottom of Waiting. The user picks a new first server after a Manual Founding Reset, as today.
- **Enroll on arrival never founds.** It applies only when the Organization already has a Cloud Pairing. In an empty Organization an auto token's hosts wait until the user enrolls the first server. This keeps the glossary rule that a waiting redemption never becomes founder automatically.

### Token model

| Token | Prefix | Lifetime | Can do | Where it lives |
|---|---|---|---|---|
| Fleet Token | `pflt_` | 1, 7 or 30 days, or until revoked | create or refresh Waiting Servers in one Organization, and read its own rows' instructions only through a check-in key signature. With Enroll on arrival, start up to N enrollments into an existing Cluster. | user_data, `/etc/ployz/fleet-token` until enrolled or stopped |
| Check-in Key | ed25519 | until enrolled or removed | sign check-ins; the waiting identity | `/var/lib/ployz/checkin/key`, 0600 |
| Per-attempt Cloud Enroll Token | `cet_` (today's) | until the attempt ends | one `/api/enroll`, single claimant, role-bound | Cloud (encrypted); the host holds it in memory only |
| Cloud Enroll Token | today's | 24h | today's `server add` | unchanged, frozen |
| Pairing Credential, Management Capability | today's | today's | today's | unchanged, issued by `/api/enroll` to the enrolling CLI process |

Expiry and revoke stop new servers. Servers already in stay. Attempts already started finish.

### Fleet Token security

Assume the token leaks. user_data is readable from the metadata service by any process on every server, including containers after join.

- **Spam.** A leaked token can fill the waiting list until it expires. Bounded by the 1000-row cap per token, per-token and per-source-IP rate limits, bulk reject, revoke and the expiry itself. Spam is noise, not power.
- **Stealing a real host's instruction.** Instructions go only to a request signed by that row's check-in key. Without the key, any token holder could post a victim's hostname and take its per-attempt token. That is why the key exists.
- **Decoy host.** An attacker checks in as `web-17` and hopes to be selected. Enrolled means a full Cluster member. This is the real threat. Mitigations follow.
  - The list and detail sheet show the source IP Cloud observed, the reported IP when it differs, provider, token name and first-seen time.
  - Glob selection prints the matched IPs, and `--all` breaks down by provider and flags Unknown.
  - The Enroll sheet warns when the selection spans providers or networks.
  - Selection is explicit unless the token enrolls on arrival.
- **Enroll on arrival.** A leaked auto token is a join token for up to N servers until it expires. It is off by default and needs a max. The Tokens page shows an Auto chip, and the CLI warns at create. Its bound is the count and the expiry, nothing else. That is weaker than today's single-claimant 24-hour token, so it ships last.
- **Per-attempt token in transit.** TLS only, released to one signed host, single claimant, expired when the attempt ends, never written to disk.
- **Replay.** The signature covers the timestamp, and skew over 5 minutes is refused. A replay can only refresh a row with stale facts.
- **Hostile report.** Decoded by Effect Schema with a 64 KiB cap, and strings are rendered as text. A lying host can only mislead its own row.
- **Probe steering.** Cloud's inbound TCP check connects to the observed IP, not the reported one. It refuses private, loopback and link-local ranges, follows no redirects and reads at most 64 bytes.
- **Revocation.** It works at once, because every check-in re-checks the token. Waiting rows from a revoked token are deleted and their loops stop on the next poll. Enrolled Servers are untouched.

### Probes

Port reachability is reported, never assumed (bet 8).

1. When the user opens a selection, Cloud hands each selected host a `ProbeRequest` with one shared nonce in its next `wait`. The host binds what it can for at most 10 seconds and sends nonce datagrams to up to three peers on UDP 51820.
2. Cloud connects in on 80 and 443 during the window.
3. The host reports `heardFrom` and `bindFailed` on its next check-in.

The detail sheet shows Open, Closed, or "Unknown · port busy". A waiting host can't be probed against an enrolled Server before WireGuard runs, so that edge shows Unknown.

### Stale rows

- Waiting rows silent for 7 days with no open attempt are deleted by the sweep. A host that comes back re-registers with the same key as a fresh row, if its token still works.
- Waiting rows of an expired token with no open attempt are deleted by the sweep.
- Rejected tombstones stay while their token lives, and are deleted with it. A host rejected under one token can come back only with a new token.
- Joined rows are deleted 7 days after `endedAt`. The Server already shows from the runtime lens.
- `attemptLifecycle`'s sweep closes runs that ended without a close.

Expiry is bookkeeping. It never touches a Cluster.

### Reconciled concepts

- **Cloud Enroll Token.** Unchanged for `server add`. A per-attempt token is the same row with `enrollmentRole` and `serverEnrollmentId` set. Users never see it.
- **Founding Claim.** Still the `organization_pairing` insert. On the waiting path Cloud assigns it by giving the founder role to exactly one token. Only legacy tokens still race.
- **Manual Founding Reset.** Unchanged.
- **Cloud Pairing, Pairing Credential, Management Capability, Management Client, Connection Candidate, Operator Allocation History.** Unchanged. They are created by the same `/api/enroll` path running in the check-in process.
- **Cloud Bootstrap Invite, Session, Token, Redemption, Waiting Redemption.** These exist only in the glossary and were never built. Delete them.
- **New glossary terms.** Fleet Token, Waiting Server, Check-in Key, Server Enrollment, Enroll on arrival.

### Module map

```
core/
  install.sh                                       PLOYZ_ENROLL_TOKEN: write token file, run checkin --install
  crates/ployzd/src/installer/assess.rs            new: HostFacts, read_facts, assess
  crates/ployzd/src/installer/{host,storage}.rs    refusals call assess; reuse or discard the ZFS build; zfs on device (last unit)
  crates/ployzd/src/installer/prepare.rs           new: `--prepare`, lock, state.json, Docker step, ZFS build ahead
  crates/ployzd/src/installer/mod.rs               `--probe --json`, `--prepare`
  crates/ployz/src/checkin/{mod,client,probe}.rs   new: loop, signed client, prep in the report, bounded probe window, unit text
  crates/ployz/src/cloud_enroll.rs                 not_yet.reason/ahead; StorageChoice device
  crates/ployz/src/handlers/server/*.rs            checkin, token, ls --waiting, show, reject, enroll
  crates/ployz/src/handlers/link.rs                `ployz status` This server block
  crates/ployz/src/handlers/verbs.rs               new verbs; `server inspect` becomes `server show`
dashboard/src/
  routes/api/checkin/index.ts                      thin: parse, verify, checkIn
  routes/api/cli/servers/{tokens,waiting,enroll-waiting}.ts
  modules/servers/waiting/tables.ts                fleetToken, waitingServer, serverEnrollment
  modules/servers/waiting/names.ts                 two-word name generator
  modules/servers/waiting/checkin.server.ts        sole writer of waitingServer
  modules/servers/waiting/enroll.server.ts         planServerEnrollment (pure), start, cancel, reject
  modules/servers/waiting/enrollment-run.server.ts attemptLifecycle run + sweep
  modules/servers/waiting/fleet-token.server.ts    create, list, revoke
  modules/machines/enrollment.server.ts            joiner guard, join gate, completed event
  modules/machines/tables.ts                       two nullable token columns
  routes/…/servers/-components/                    add-servers dialog, tokens page, waiting list, enroll sheet, enrolling list
docs/user/servers/add-many-servers.md              new
core/CONTEXT.md, dashboard/CONTEXT.md              glossary
```

The host's whole interface is one install line and one exit code. Cloud's interface is one route with three answers plus a handful of server functions. Founder choice and skip reasons stay inside the pure planner.

### Migration

There is no data migration.

- The three new tables start empty.
- `machine_enrollment_token` gains two nullable columns, and null keeps today's behavior.
- The glossary loses the Cloud Bootstrap terms.
- Existing Servers, today's tokens and `server add` don't change.
- `server inspect` is renamed to `server show` with no alias. The CLI has no users past v0.2.0 to break.
- An older CLI never calls `/api/checkin`. An older daemon reads nothing new.

## Synthesis decision

The panel was meant to have one Opus runner and one Codex runner. Both Codex launches through `delegate_task` failed with `parent_not_active`, including a retry with a fresh clientRequestId. The Codex CLI path is unusable on codex-vm. So the Codex member was skipped, and two Opus runners ran with opposing seeds.

- **A, CLI check-in pull loop.** The host pulls instructions from Cloud.
- **B, Cloud dials a pre-join daemon over iroh.** It uses a scoped `enroll` Management Client and new `InspectHost`, `PrepareMachineStorage` and `ProbePorts` daemon routes.

**A is the base.** It reuses today's `/api/enroll` → `session.register` → join path unchanged, in-process. The frozen enrollment surface gains only optional fields, and the daemon doesn't change.

B's costs.

- It crosses bet 10 with three daemon routes.
- It makes the daemon interpret a Management Client label for the first time, a hidden meaning in a free-text field.
- It moves ZFS preparation out of the installer into a daemon route.
- It puts onboarding behind relay reachability.
- It rebuilds the founder and joiner sequence in a new Inngest run instead of reusing the one that works.

B's batching has a concrete flaw. Its `concurrency: 10` key doesn't bound joins, because Inngest counts runs that are executing steps, not runs parked in `step.sleep` or `waitForEvent`. B's `waitParticipating` parks in exactly that way. A's derived count under the existing pairing lock bounds the actual join.

Grafted from B.

- Enroll on arrival never founds.
- Report size cap, observed-IP-only inbound probe with private-range refusal, and spread warnings on selection.
- A bounded probe window instead of A's always-on UDP responder.
- Glob selection prints matched IPs before acting.

Rejected from B. A persisted `state` column. A derives the state, which avoids a hand-synced copy.

## Tradeoffs accepted

- Up to 10 seconds between Enroll and a host starting, in exchange for no inbound reachability and no persistent connection. A long-poll can cut this later without a wire change.
- A CLI loop that may run for days on a waiting host, in exchange for no daemon change and a host you can fix and recheck without a new token. The loop ends at enroll, reject, revoke or expiry.
- Preparation installs Docker before anyone approves the host. A rejected host keeps Docker. The hint says so up front.
- Preparation also installs ZFS build deps and builds the module before anyone picks storage. A host that enrolls with Docker volumes pays the build time and disk until enroll throws it away. That buys an enroll that doesn't wait 20 minutes for a ZFS build on Debian and Amazon Linux.
- The founder waits for its own preparation. An Organization's first server on Debian with ZFS can't join before its build finishes.
- Cloud trusts the host's own facts, in exchange for one assessment function. A lying host only misleads its own row.
- A legacy `server add` can still race a designated founder, in exchange for leaving the frozen path alone.
- A check-in key that isn't the future Management Identity, in exchange for no `machine.json` seeding. The two are linked through the per-attempt token's `claimedMachineId`.
- A new, small, versioned check-in protocol, which `/api/checkin` freezes from day one. It has `protocolVersion`, optional fields, and an unknown `kind` reads as wait.

## Alternatives considered

- **Cloud dials a pre-join daemon (candidate B).** Rejected for the reasons above. Its strongest point is that no long-lived client runs on the host, but here a systemd-supervised CLI loop costs less than three daemon routes and a meaningful label.
- **Batching with Inngest concurrency or a wave controller run.** Concurrency doesn't count parked runs. A controller run owns many resources, against one run per resource.
- **Run the full installer, ZFS included, before waiting.** That commits storage before the user chooses it in the Enroll step.
- **Install ZFS during preparation and remove it when storage is `none`.** It loads a kernel module and installs packages on hosts that may never use them, and removal has to undo a loaded module. Building without installing leaves nothing loaded, so throwing it away is `dkms remove` and a directory.
- **Install Docker before the loop starts, then check in.** The host stays invisible for the length of the Docker install, and a failed install fails the install line with nobody watching.
- **Token rules for provider, source CIDR or hardware.** Dropped. Hardware fit is already checked per host at enroll. Provider and CIDR rules add a policy language to a credential whose real bounds are a count and an expiry.
- **A separate check-in binary or shell script.** A script can't reuse `assess`, and a second binary is a second release artifact.
- **Full `cloud` capability at check-in.** A leaked waiting row would hold full Machine power.

## Bets and red flags

- **Stable promise.** Additive only.
  - `/api/enroll` gains optional `not_yet.reason`, `not_yet.ahead` and storage `device`.
  - `machine_enrollment_token` gains nullable columns.
  - There is no daemon RPC change.
  - `/api/checkin` is new and frozen from birth.
  - `ployz.sh` keeps working with no `PLOYZ_ENROLL_TOKEN`.
- **Bet 3, bounded commands.** Red flag crossed. The check-in loop outlives `--install`. It is a host-local loop that observes and reports, it states its bound, and its one effect (`server add`) is a bounded command the user asked for in Cloud. Preparation also runs after `--install` exits. It is one oneshot unit with a lock and a fixed list of steps, and it ends on its own.
- **Bet 6, Machine-local authority.** Cloud acts as the operator for founder, names and join slots. The host still refuses any storage its own `assess` rejects. The join gate is operator patience, not a safety wait.
- **Bet 7, Cloud drives, never owns.** Red flag adjacent. Cloud steers a host over HTTP pull rather than Machine RPC. A waiting host isn't a Machine and holds no Cluster state, and the instruction is one closed, typed verb. If Cloud vanishes, the host waits and no Cluster is touched.
- **Bet 8, evidence.** Facts come from the host, probes report Unknown when they can't prove reachability, and IPs are the ones Cloud observed.
- **Bet 10, client-first.** Held. All new host behavior lives in the CLI and the one-shot installer.
- **dashboard/DESIGN.md destructive actions.** Reject and revoke delete no Server, so the dashboard doesn't ask.

## Open risks

1. **`zfs:<device>` and frozen formats.** Does `ployzd` record the pool's backing anywhere frozen (`machine.json`, replicated rows)? If it does, device pools need an audit first. `machine_pool::one_usable` accepts any single imported pool, which suggests not.
2. **`JOIN_SLOTS = 8`.** This is a guess. Measure founder CPU and Corrosion sync per batch on the first 20+ host lab run before fixing it.
3. **A re-imaged host.** It gets a new check-in key, so a rejected host that is wiped and reinstalled with the same live token comes back as a new row. "They won't come back" holds per install, not per machine.
4. **DKMS reuse at enroll (UNVERIFIED).** See Host preparation. If the package install recompiles the module, enroll falls back to `dkms install` on the prebuilt module. C7 owns the Boat lane that decides it.
5. **Preparation cost on small hosts.** A ZFS build uses CPU and disk while the host waits. A host that will never run ZFS still pays it until enroll. If this bites, the Enroll sheet or the token can later say "No ZFS" and skip the build.
6. **`ployz status` on a host.** It already prints scope status for a signed-in user. On a waiting host there is no sign-in, so the This server block must print first and the scope lines must degrade quietly.

## Build units

Each unit is one PR with its own check. Units run in this order. A unit may start once everything it depends on has merged.

| Id | Unit | Side | Depends on | Check |
|---|---|---|---|---|
| C1 | `installer/assess.rs` with `HostFacts`, `read_facts`, `assess`, and `ployzd install --probe --json`. Today's refusals route through it unchanged. | core | none | `cargo nextest run -p ployzd assess` passes a test per blocker, and `probe --json` on a Boat host prints the facts. |
| C2 | `ployzd install --prepare` as one locked, idempotent background job. It installs Docker, then builds ZFS ahead without installing it (Ubuntu downloads only, Debian and Amazon Linux build with DKMS), and writes `state.json`. | core | C1 | A Debian Boat host ends with Docker running, a DKMS-built module, nothing in `/lib/modules/<kernel>/updates`, no ZFS package, no `ployzd` service and no pool. |
| D1 | `fleet_token` table, two-word names, expiry, `createFleetToken`, `listFleetTokens`, `revokeFleetToken`, and the CLI API routes. | dashboard | none | A Postgres vitest shows an expired token refused, a name collision retried, and revoke deleting only waiting rows with no open attempt. |
| D2 | `waiting_server` table, `POST /api/checkin` answering `wait` and `stop`, signature check, the optional `prep` field, `rejectWaitingServers`. | dashboard | D1 | Route tests cover a bad signature (401), an expired token (`stop{token_expired}`), a rejected key (`stop{rejected}`), and a request with and without `prep`. |
| C3 | `ployz server checkin` loop, signed client, both systemd units, starting preparation, `prep` in each check-in, `status.json`, and the This server block in `ployz status`. | core | C1, C2, D2 | On Boat against `SERVERS=1` Cloud, a host shows in the waiting list within 10 s of `checkin --install` exiting, its status moves from Installing Docker to Ready, and reject stops both units. |
| C4 | `install.sh` reads `PLOYZ_ENROLL_TOKEN` and `PLOYZ_SKIP_ZFS`, writes the token file and runs `checkin --install`. Without it, behavior is unchanged. | core | C3 | `core/scripts/test-cli-installer.sh` covers both paths, and one `curl \| sh` on Boat lands a waiting row. |
| C5 | `ployz server token create\|ls\|revoke` with `--name`, `--auto --max`, `--expires`. | core | D1 | A `cli_shape.rs` case per verb, and the revoke output reports the removed count against `SERVERS=1`. |
| C6 | `ployz server ls --waiting`, `server show` (renamed from `server inspect`, covering waiting rows), `server reject` with glob and confirm. | core | D2 | `cli_shape.rs` cases, and a glob reject on two Boat hosts confirms then stops both. |
| D3 | UI for the Add servers dialog with Shell, cloud-init and Terraform tabs, the Tokens page, and the Waiting list with filters, detail sheet, reject, and preparation in the status cell. | dashboard | D1, D2 | verify-dashboard with `SERVERS=1` shows a minted token, a host arriving live, its status moving from Installing Docker to Ready with no row or column moving, and fixed columns with the 10-row box. |
| D4 | `server_enrollment` table, `planServerEnrollment`, start and cancel, the run through `attemptLifecycle`, the joiner guard, the completed event, and the `enroll` answer held until the host is ready for its storage. | dashboard | D2 | A Postgres vitest plans a founder plus joiners and refuses two founders, and a check-in with `prep.docker: installing` gets `wait` for an instructed attempt. The legacy `server add` tests stay green. |
| C7 | The loop runs the `enroll` instruction in-process through the `server add` body, reports stages, and handles `not_yet{queued}`. The ZFS install reuses the prepared build, and storage `none` throws it away. | core | C3, D4 | Boat with three hosts. One founder and two joiners all join, a forced failure returns a host to waiting, a Debian host enrolled with storage `none` keeps no DKMS module, cache or added build deps, and the UNVERIFIED DKMS reuse is settled on Debian. |
| D5 | Join gate with the derived `joining` count, `not_yet.reason` and `ahead`. | dashboard | D4 | A test with `JOIN_SLOTS = 1` and three joiners shows serialized joins. |
| C8 | `ployz server enroll` with globs, `--name 'web-{n}'`, `--first`, `--all` provider breakdown, `--status`, and the storage flags. | core | C6, C7 | `cli_shape.rs` cases, and a glob enroll on three Boat hosts prints the skip list and progress. |
| D6 | UI for the Enroll sheet (first server, volumes, names, runs, spread banner) and the Enrolling list with six fixed counters, append order, joined rows that stay, and sent-back rows at the bottom of Waiting. | dashboard | D3, D4, D5 | verify-dashboard with `SERVERS=1` and three hosts. Screenshots before and after each stage change show no row or column moving. |
| D7 | Enroll on arrival. `autoEnrollMax`, the default plan, the used count, ready hosts only, and never founding. | dashboard | D4 | A Postgres vitest with max 2 and three hosts enrolls two and leaves the third waiting. An empty Organization enrolls none. A host still preparing starts no attempt until it is ready. |
| D8 | Inbound probes on 80 and 443 with private-range refusal, and the probe request in `wait`. | dashboard | D2 | A route test refuses a private observed IP. |
| C9 | The bounded probe window on the host. | core | C3, D8 | A Boat pair with UDP 51820 blocked on one host shows Closed for that edge. |
| C10 | `--storage zfs:<device>` in the installer and the instruction. | core | C7, open risk 1 | A Boat host with a spare disk builds its pool on that disk. |

C1 through D3 give a read-only waiting list, with preparation running, before anyone can enroll from it. D7 ships after D6 because enroll on arrival is the weakest credential. User docs ride with the unit that changes them. C4 updates `docs/user/servers/add-a-server.md`, and D6 adds `docs/user/servers/add-many-servers.md`.
