# VM Lab

The VM lab exercises Ployz from a cloud-like starting point:

- thin distro cloud images
- cloud-init guest seeding
- no preinstalled Ployz binaries
- bootstrap/install under test
- `HostService` runtime mode
- reusable cached bases for fast iteration

The lab runs from your main machine and uses a dedicated Linux workstation as the
libvirt/qemu host.

## Host Setup

1. Copy [`lab/lab.env.example`](../../lab/lab.env.example) to `lab/lab.env`.
2. Set `PLOYZ_LAB_HOST` to your Linux workstation SSH target.
3. If the host uses passworded `sudo`, set `PLOYZ_LAB_HOST_SUDO_PASSWORD` in your
   shell before running `host bootstrap`.
3. Run:

```bash
PLOYZ_LAB_HOST_SUDO_PASSWORD='your-sudo-password' just lab host bootstrap
just lab image build ubuntu
just lab image build rocky
just lab image build arch
```

`host bootstrap` prepares:

- libvirt/qemu tooling
- optional `apt-cacher-ng` for Ubuntu guest package caching on the `mgmt` network
- a storage pool under `PLOYZ_LAB_HOST_ROOT`
- two libvirt networks
  - `mgmt`: SSH, artifacts, console access, MTU below `1280`
  - `data`: mesh traffic and fault injection
- Rust tooling on the workstation so the current branch can be built there

## Base Types

The lab now supports three VM bases:

- `raw`: the upstream cloud image for a distro family
- `bootstrapped-pristine`: first boot complete, cloud-init complete, SSH working, but still no Docker, no `ployzd`, no service unit, and no Ployz data dir
- `post-install`: `ployzd`, `ployz`, `ployz-gateway`, `ployz-dns`, and `corrosion` installed via `ployz.sh`, `ployzd.service` enabled and active, and reboot survival verified

Build cached bases explicitly:

```bash
just lab snapshot build bootstrapped-pristine --family ubuntu
just lab snapshot build post-install --family ubuntu
just lab snapshot list
```

Default behavior:

- `bootstrap test` uses `raw`
- `scenario run` prefers `post-install`
- `suite run semantics` and `suite run fault` use `post-install`
- `suite run bootstrap` stays on `raw`

## Ubuntu Apt Cache

The lab can run an `apt-cacher-ng` instance on the workstation and point Ubuntu
guests at it during bootstrap.

Config knobs in `lab/lab.env`:

- `PLOYZ_LAB_APT_CACHE_ENABLED=1`
- `PLOYZ_LAB_APT_CACHE_PORT=3142`
- `PLOYZ_LAB_APT_CACHE_IMAGE=sameersbn/apt-cacher-ng:latest`

When enabled:

- `host bootstrap` uses a native `apt-cacher-ng` package when available
- on Omarchy/Arch, `host bootstrap` falls back to a Docker-backed apt cache
- Ubuntu `cloud-init` bootstrap uses the proxy automatically
- Ubuntu SSH bootstrap stages a payload and runs `ployz.sh install --source payload --mode host-service`
- run metadata records the proxy URL in `.lab/reports/<run-id>/metadata.env`

This keeps guests pristine while reducing repeated upstream apt mirror fetches on
bad networks.

### Omarchy Notes

The current lab host path has been verified on Omarchy/Arch with these details:

- libvirt is managed via the system daemon at `qemu:///system`
- `host bootstrap` adds the remote operator to `libvirt` and `kvm`
- the harness defines and starts the `ployz-lab` storage pool plus
  `ployz-lab-mgmt` and `ployz-lab-data` in system libvirt
- when the host requires a password for `sudo`, the most reliable invocation is:

```bash
PLOYZ_LAB_HOST_SUDO_PASSWORD='your-sudo-password' just lab host bootstrap
```

After a successful run, these checks should pass on the workstation:

```bash
id -nG "$USER"
sudo virsh --connect qemu:///system pool-info ployz-lab
sudo virsh --connect qemu:///system net-info ployz-lab-mgmt
sudo virsh --connect qemu:///system net-info ployz-lab-data
```

## Bootstrap Contract

The installer entrypoint under test is [`ployz.sh`](../../ployz.sh).

It supports:

- `install --source release|git|payload`
- `--mode docker|host-exec|host-service`
- `--payload-dir PATH`
- `--no-daemon-install`

Its contract in v1 is:

- install `ployzd`, `ployz`, `ployz-gateway`, `ployz-dns`, and `corrosion` into user space
- write an install manifest and client config
- run `ployz daemon install --mode ...`
- for lab, promote into `HostService`
- remain safe to rerun

[`scripts/bootstrap-linux.sh`](../../scripts/bootstrap-linux.sh) remains as a compatibility shim and maps legacy lab flows onto `ployz.sh install --source payload --mode host-service`.

Supported families in v1:

- Ubuntu
- Rocky/RHEL-like
- Arch

## Day-to-Day Flow

Bootstrap validation:

```bash
just lab bootstrap test cloud-init --family ubuntu
just lab bootstrap test ssh --family ubuntu
just lab bootstrap test ssh --family ubuntu --base bootstrapped-pristine
```

Scenario runs:

```bash
just lab scenario run founder_init_after_bootstrap --family founder=ubuntu
just lab scenario run machine_add_basic --family founder=ubuntu,joiner=rocky --profile regional
just lab scenario run replace_machine --family founder=ubuntu,joiner=rocky,replacement=arch --profile far --base raw
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
Each report includes the selected base, per-node backing image paths, and copied
base metadata under `.lab/reports/<run-id>/bases/`.

If your lab host requires `sudo`, export `PLOYZ_LAB_HOST_SUDO_PASSWORD` in the
same shell before running bootstrap or scenario commands that need first-time
host preparation. Do not commit that value into `lab/lab.env`.

## What the Harness Verifies

Bootstrap tests:

- raw guest starts without Docker or `ployzd` preinstalled
- bootstrap installs binaries and service
- `bootstrapped-pristine` preserves the pre-install contract
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

- `scenario run` fails fast if a required `post-install` base is missing; build it
  first with `just lab snapshot build post-install --family <family>` or override
  with `--base raw`.
- `post-install` reuse is allowed across git revisions, but the harness warns when
  the cached base was built from an older commit.
- The harness intentionally uses the current product semantics as-is. If a scenario
  fails because the product does not yet expose a clean disable path before removal,
  that failure is part of the signal.
- The default image URLs in `lab/lab.env.example` are overridable because upstream
  cloud image locations can change.
