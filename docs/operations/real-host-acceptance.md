# Real-Host Acceptance

`scripts/real-host-acceptance.sh <core-ip> <edge-ip>` is the release-candidate
capstone that DinD cannot replace: real tcx eBPF, WireGuard, mixed architecture,
host firewalls, public DNS/TLS, the public installer, and real ZFS kernel and
reboot behavior on two fresh machines.

It proves this flow:

1. Install Ployz and form the cluster on the core.
2. Join the edge.
3. Build one exact commit with Dockerfile and Railpack for both release
   architectures and retain stable operation evidence.
4. Deploy an image-based Compose app with one replica on each machine.
5. Receive and serve its managed public HTTPS URL.
6. Route through both gateways, including to the replica on the other machine.
7. Restart the control daemon without interrupting the public route.
8. With explicit destructive certification enabled, prepare ZFS on the Rocky
   core, deploy PostgreSQL on a Provisioned Volume, write a unique row, reboot,
   and prove the same container data and dataset return.
9. Quarantine the Rocky ZFS module, reboot, and prove storage is unavailable,
   the pinned database cannot start against an empty directory, the stranded
   pin is named, and the control plane remains healthy. Restore the module and
   prove the data returns again.

The fixed matrix covers both release architectures and both managed firewall
backends:

| Role | OS | Architecture | Firewall | Storage certification |
| --- | --- | --- | --- | --- |
| core | Rocky Linux 9 | amd64 (`x86_64`) | active firewalld | ZFS preparation, baseline/failure/recovery reboots |
| edge | Ubuntu 24.04 | arm64 (`aarch64`) | UFW enabled by the harness | cluster join, native build, cross-machine route |

Use fresh hosts. The harness rejects another OS/architecture pair, requires
root SSH to both public IPs, and leaves the machines running for inspection.
The operator machine needs Bash 4.4 or newer, Git, SSH, curl, Python 3, GNU
coreutils `timeout`, and its key in `ssh-agent`. The core needs enough free
space for the Ployz-owned ZFS backing file and PostgreSQL image and data; do
not use a production or shared host.

## Seal the public candidate

The certification path installs an immutable public tag; an unpublished local
binary does not satisfy this gate. The tag must resolve exactly to the sealed
runtime SHA, and that SHA must contain commit
`2f754ab5cff785fd67cf4c83231f4025ec6ad8ee` at minimum. Start
from a clean local checkout: the harness records its separate harness SHA but
does not require it to equal the runtime SHA.

Follow [Release Operations](release.md) to publish the exact `v*` tag, verify
all release assets, and promote that same tag to alpha. Record these commands
and their output before provisioning hosts:

```sh
tag=<immutable-v-tag>
runtime_sha=<40-lowercase-hex-sealed-sha>

test "$(git rev-list -n 1 "${tag}")" = "${runtime_sha}"
git merge-base --is-ancestor \
  2f754ab5cff785fd67cf4c83231f4025ec6ad8ee "${runtime_sha}"
scripts/verify-release-assets.sh "${tag}" --print-channel
curl -fsSL https://ployz.sh/channels/alpha.env
```

The downloaded alpha channel must name `tag`, its release manifests and
assets must pass verification, and the release/tag must be immutable before
the run begins. After installation, the harness records and verifies the
installed release tag and immutable manifest URL on both hosts. Record those
values in the evidence template; do not proceed merely because
`ployz --help` works.

## Provision least-privilege hosts

Create a Rocky 9 amd64 core and Ubuntu 24.04 arm64 edge in the same region
when possible. Use a dedicated, short-lived SSH key for this run. Add that key
only to these two hosts, load it into `ssh-agent` for the run, and revoke it
after evidence collection. Do not reuse a personal or production automation
key.

Provider firewall rules should allow inbound traffic only from the narrowest
source that can exercise each seam:

- `22/tcp` from the operator's current address;
- `4222/tcp` and `51820/udp` between the two host addresses;
- `80/tcp` and `443/tcp` from the public internet for DNS/TLS acceptance.

Keeper owns the corresponding guest firewalld/UFW rules. Do not pass
`--host-ports-assured-externally`; the harness verifies runtime and persistent
firewalld rules and UFW rules. Record provider ids, image ids, addresses,
firewall rule ids, region, volumes, snapshots, rescue settings, and the
ephemeral SSH key fingerprint in the evidence inventory.

Before the destructive phase, confirm provider rescue-console access works
without relying on the guest network or guest SSH daemon. Record the provider
recovery procedure and console identifier, but do not place provider tokens in
the evidence directory.

## Run the ordinary capstone

Without the destructive opt-in, behavior remains the ordinary mixed-host
capstone:

```sh
scripts/real-host-acceptance.sh <core-ip> <edge-ip>
```

It verifies authenticated exact-commit builds for Dockerfile and Railpack,
both native platforms, cancellation evidence, firewall ownership, managed
HTTPS, cross-machine routing, and control-daemon restart invisibility. Any
failed command or non-200 response exits non-zero. Keep the full transcript.
For the sealed #391 gate, also run the CLI smoke path against the cluster left
by this run and retain its transcript:

```sh
scripts/cli-smoke-test.sh <core-ip> <edge-ip>
```

## Run the destructive ZFS certification

The certification phase intentionally prepares storage, writes test data,
changes the Rocky host's boot-visible module files, and reboots it. It is
disabled unless all four environment guards below are present. The evidence
directory must be an absolute, initially empty directory on durable operator
storage; never place it on either test host.

```sh
evidence_dir=<absolute-empty-operator-path>

PLOYZ_REAL_HOST_ZFS_CERTIFY=1 \
PLOYZ_REAL_HOST_EVIDENCE_DIR="${evidence_dir}" \
PLOYZ_EXPECTED_RELEASE_TAG="${tag}" \
PLOYZ_EXPECTED_RUNTIME_SHA="${runtime_sha}" \
  scripts/real-host-acceptance.sh <core-ip> <edge-ip>
```

These names are the exact script interface. Do not alias, omit, or synthesize
them in a wrapper. Before accepting destructive consent the script verifies a
clean harness checkout, the tag-to-runtime-SHA equality, the minimum ancestor,
the fixed host matrix, the installed public release, and an empty evidence
destination. Rescue-console readiness remains an operator prerequisite and
must be recorded separately before invocation.

Before provisioning, run the harness's deterministic local phase regression:

```sh
scripts/real-host-acceptance.sh --self-test
```

It uses no network or host mutation. It proves a failed assertion terminates a
phase before the next command and a successful phase can pass captured identity
to the next phase while its output is both streamed and retained.

The evidence root contains `metadata.env`, `live-alpha.env`, `transcript.log`,
`sealed-harness.sh`, `commands.log`, `recovery.txt`,
`zfs-module-recovery.env`, numbered phase logs, and `sha256sums`. Copy
[`real-host-zfs-536-391-evidence.md`](real-host-zfs-536-391-evidence.md) beside
them and fill it from those artifacts. A successful run prints:

```text
ZFS REAL-HOST CERTIFICATION PASSED
```

That marker is necessary but not sufficient: inspect every assertion below
and retain the artifacts before reporting the gate green.

## Provisioned PostgreSQL and reboot assertions

The harness prepares storage through the public control-plane operation, not
by calling hidden Host Runner commands. Preserve the operation id, full
`ployz ops watch <operation-id> --json` output, and `ployz machine inspect`
testimony. Storage must be `ready`, and the prepared descriptor, pool, dataset
root, quota, and mountpoint must agree with live `zpool`/`zfs` output.
The descriptor must be `/var/lib/ployz/prepared-storage.json` with owned-image
origin `/var/lib/ployz/zfs/ployz.img`. The harness records the descriptor hash
and the backing file's resolved path, filesystem/inode identity, and size, then
requires them to remain identical after every reboot and during failure.
Baseline and every healthy reboot also require parseable pool health to be
exactly `ONLINE`.

The PostgreSQL fixture declares `x-ployz.max-size: 2G`, which is exactly
2,147,483,648 bytes. The live dataset must report `quota=2147483648` and
`refquota=0` in parseable output; record both at baseline and require both values to remain
unchanged after the normal reboot and each recovery reboot. The fixture pins
the Provisioned Volume to the Rocky core and uses a unique row value derived
from the run id. Record the deploy operation and the exact dataset shown by
`ployz volume list`. Before reboot, prove all of the following:

- the database container uses the expected `/var/lib/ployz/volumes/...`
  dataset mount rather than a plain Docker volume or directory;
- every required `plz.*` recovery label names the expected namespace and
  service, and the complete label map remains equivalent across every phase;
- `zfs` reports that exact dataset with its expected mountpoint, quota, and
  refquota;
- PostgreSQL returns the unique row;
- the dataset and container identifiers, row marker, pool health, and storage
  testimony are captured.

After a normal Rocky reboot, wait through absolute monotonic SSH and service
readiness deadlines. Each SSH attempt made by those retry loops is locally
terminated within 15 seconds, so a remote transport timeout cannot extend the
overall retry-loop deadline. Prove that
ZFS is imported before Docker starts the workload, the
same dataset identity and mountpoint exist, the database returns the same row,
and a fresh `ployz machine inspect` reports `storage ready pool=<pool>`. The
harness reissues that inspection within its bounded readiness window; stale or
wrong-pool output does not satisfy the check. A newly empty database, a
different dataset, a recreated plain directory, or a manual `zpool import` is
a failure even when the query later succeeds.

## Reversible module-failure assertion

The harness records the running kernel, the exact loadable module file reported
by `modinfo -n zfs`, that file's checksum, mode and owner, loaded modules, ZFS
units, and whether the active initramfs contains ZFS before changing anything.
Failure to run `lsinitrd` is distinct from a successful inventory containing
ZFS; either condition stops the phase. The harness then writes a recovery
manifest, creates and verifies a copy outside boot module search paths, copies
the manifest locally, and prints emergency recovery instructions before moving
the module. A separate fail-closed `set -euo pipefail` transaction quarantines
the loadable file, runs `depmod` for the recorded kernel, and checks that
`modinfo` can no longer discover ZFS before reboot.
`zfs-module-recovery.env`, `recovery.txt`,
and the checksum manifest in the recorded remote recovery root together are
the authority for restoration; do not improvise different paths from memory.

After reboot with the module absent, the certification must prove all of these
at the same time:

- the core control-plane services remain active and bounded commands still
  answer;
- fresh machine testimony renders storage as
  `unavailable zfs-module-missing`, corresponding to typed
  `Unavailable { ZfsModuleMissing }`;
- the storage alarm names the exact stranded namespace/volume pin and its
  machine, rather than merely reporting a generic machine failure;
- ZFS is not loaded and the pool/dataset are not available;
- Docker does not start the database against a newly created empty directory;
- the expected database container is stopped or fails loudly, and no
  replacement container serves an empty PostgreSQL instance;
- the preserved backing file, prepared descriptor, dataset identity, Docker
  labels, operation evidence, and last-known row marker remain available for
  diagnosis.

Do not treat SSH loss alone as proof. Use the provider rescue console when the
guest cannot be reached, preserve console output, and diagnose boot, network,
systemd, Docker, ZFS, Ployz control, and machine-role state separately.

## Recovery and diagnosis

On an SSH-reachable host, follow the exact restoration commands recorded in
`recovery.txt`: restore the quarantined module file to its recorded path,
owner and mode, verify its hash, run `depmod` for the recorded kernel, and
verify `modinfo` discovers ZFS before rebooting. The normal path does not need
to rebuild an initramfs that the preflight proved did not contain ZFS.
Restoration is safe to retry: an already exact destination is reused, while an
invalid destination is replaced atomically from the verified backup without
removing the backup or quarantine evidence.

If SSH is unavailable:

1. Open the pre-verified provider rescue console and capture the failure.
2. Boot the provider rescue image without formatting or recreating disks.
3. Mount the original root filesystem read-write at a temporary recovery
   mount. Do not import the Ployz ZFS pool or mount its dataset merely to make
   the machine appear healthy.
4. Set `PLOYZ_RECOVERY_MOUNT_ROOT` to the canonical absolute mountpoint and run
   `recovery.txt`. It rejects a symlink/non-mount root, verifies the original
   machine id and kernel module tree, and applies every recorded path beneath
   that mounted root while preserving the module file's ownership, mode, and
   hash.
5. The recorded recovery commands chroot into the mounted original root for
   `depmod` and `modinfo`, so both operate on the original kernel tree rather
   than the rescue image. Do not rebuild initramfs: certification preflight
   proved that the active initramfs did not contain ZFS.
6. Reboot the original system and continue the scripted recovery assertions.

After recovery, require the module, pool, exact dataset with its unchanged
quota/refquota, testimony, alarm clearance, PostgreSQL container, and original
unique row to return. Keep the failure and recovery evidence even after the
final state is healthy.
The exit trap continues to print emergency recovery instructions until the
restoration, recovery assertions, final reboot, and evidence checks all
succeed; an intermediate healthy observation never clears that warning.

Never run `zpool create`, `zpool destroy`, `zfs destroy`, `ployz volume rm`,
Docker volume deletion, backing-file truncation, host re-provisioning, or
provider disk deletion while diagnosing. Do not remove quarantine copies,
database data, containers, or operation evidence until the row is recovered
and all evidence has been copied to durable operator storage.

## Tear down and retain evidence

The harness never deletes hosts. Once the filled checklist has been reviewed
and data recovery is proven, tear down every item in the recorded inventory:

- both servers and any attached disks, snapshots, rescue settings, and
  placement groups;
- provider firewall rules created for the run;
- ephemeral DNS records and certificate/lease resources;
- the dedicated SSH key from the provider and local `ssh-agent`;
- temporary Git fixture service, certificate, trust anchors, and credentials;
- test branch or other fixture resources governed by the hosted acceptance
  runbook.

Retain the filled checklist, immutable candidate metadata, transcript,
numbered phase logs, recovery files, console evidence, commands, timings, and
redacted provider inventory for the release's evidence-retention period. Do
not retain SSH private keys, Git credentials, provider tokens, Cloud Bootstrap
Tokens, or unredacted environment files.
