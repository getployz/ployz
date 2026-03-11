# VM Lab

The VM lab exercises Ployz from a cloud-like starting point:

- thin distro cloud images
- cloud-init guest seeding
- no preinstalled Ployz binaries
- bootstrap/install under test
- `HostService` runtime mode

The lab runs from your main machine and uses a dedicated Linux workstation as the
libvirt/qemu host.

## Host Setup

1. Copy [`lab/lab.env.example`](../../lab/lab.env.example) to `lab/lab.env`.
2. Set `PLOYZ_LAB_HOST` to your Linux workstation SSH target.
3. Run:

```bash
just lab host bootstrap
just lab image build ubuntu
just lab image build rocky
just lab image build arch
```

`host bootstrap` prepares:

- libvirt/qemu tooling
- a storage pool under `PLOYZ_LAB_HOST_ROOT`
- two libvirt networks
  - `mgmt`: SSH, artifacts, console access, MTU below `1280`
  - `data`: mesh traffic and fault injection
- Rust tooling on the workstation so the current branch can be built there

## Bootstrap Contract

The installer entrypoint under test is [`scripts/bootstrap-linux.sh`](../../scripts/bootstrap-linux.sh).

It supports:

- `--artifacts-dir PATH`
- `--artifacts-url URL`
- `--mode host-service`

Its contract in v1 is:

- install OS prerequisites
- install `ployz`, `ployzd`, and `corrosion`
- install and enable `ployzd.service`
- start the daemon in `HostService`
- remain safe to rerun

Supported families in v1:

- Ubuntu
- Rocky/RHEL-like
- Arch

## Day-to-Day Flow

Bootstrap validation:

```bash
just lab bootstrap test cloud-init --family ubuntu
just lab bootstrap test ssh --family ubuntu
```

Scenario runs:

```bash
just lab scenario run machine_add_basic --family founder=ubuntu,joiner=rocky --profile regional
just lab scenario run replace_machine --family founder=ubuntu,joiner=rocky,replacement=arch --profile far
```

Suite runs:

```bash
just lab suite run bootstrap
just lab suite run semantics
just lab suite run fault
just lab suite run full
```

Reports:

```bash
just lab report open <run-id>
```

Reports are stored under `.lab/reports/<run-id>/`.

## What the Harness Verifies

Bootstrap tests:

- raw guest starts without Docker/Ployz preinstalled
- bootstrap installs binaries and service
- `systemctl is-enabled ployzd`
- `systemctl is-active ployzd`
- reboot survival

Semantic scenarios:

- founder init
- machine add
- machine remove guard behavior
- replacement flow
- rejoin after remove

Fault scenarios:

- latency and jitter on the data NIC
- full data-plane partition
- SSH bootstrap under impaired management connectivity

## Current Limits

- The repo does not currently ship a dedicated `ployz` Rust binary, so v1 installs
  a thin wrapper at `/usr/local/bin/ployz` that delegates to `ployzd`.
- The harness intentionally uses the current product semantics as-is. If a scenario
  fails because the product does not yet expose a clean disable path before removal,
  that failure is part of the signal.
- The default image URLs in `lab/lab.env.example` are overridable because upstream
  cloud image locations can change.
