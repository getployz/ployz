# Real-Host Acceptance

Two scripts prove ployz on real cloud hosts (the DinD harness covers the same
product flow locally; this exercises tcx eBPF, real WireGuard, and the public
install path, which containers can mask):

- `scripts/real-host-acceptance.sh <core-ip> <edge-ip>` — the beta-gate proof:
  form a two-machine cluster, deploy an image-based service across both
  machines, and check cross-machine routing and daemon-restart invisibility.
  Prints per-step timing.
- `scripts/cli-smoke-test.sh <core-ip> <edge-ip>` — run every realistic CLI
  command (happy and unhappy paths) and print each with its real output and
  exit code. Forms the cluster if it is not already up, so it runs standalone
  or against the cluster the acceptance script just left.

Both install the **public alpha channel** from `https://ployz.sh`, so they test
whatever `machine init` currently resolves — cut/promote a release first if you
need to test unreleased `main` (see `docs/operations/release.md`; `machine init`
has no `--version`, so the alpha channel must point at the build under test).

## Host requirements

- Two **fresh** Ubuntu 24.04+ **amd64** hosts, kernel **6.6+** (tcx eBPF
  attach). 1 GB / 1 vCPU is enough — a full run uses ~410 MB and does not OOM.
- Run the scripts from a machine with **root SSH to both hosts** (key in your
  ssh-agent). If your local uplink to the hosts is flaky, run them from a small
  VM that has stable SSH to the pair.
- Ports left open: `22`, `80`, `443`, `4222/tcp`, `51820/udp`. Cloud images with
  no host firewall (the default on Vultr/Hetzner) need nothing extra.

amd64-only today: the arm64 half of the mixed-arch gate is pending Hetzner
Ampere stock, and RHEL-family hosts are blocked by the `apt-get` hardcode
(getployz/ployz#402).

## Provision two hosts

Vultr (cheapest; `vc2-1c-1gb`, ~\$0.007/hr each):

```sh
KEYS=<ssh-key-id>          # vultr-cli ssh-key list
for n in ployz-core ployz-edge; do
  vultr-cli instance create --plan vc2-1c-1gb --region fra --os 2284 \
    --ssh-keys "$KEYS" --label "$n" --host "$n"
done
vultr-cli instance list    # grab the two IPs once STATUS is active
```

Hetzner (native mixed-arch when ARM returns; `cpx22` amd64):

```sh
for n in ployz-core ployz-edge; do
  hcloud server create --name "$n" --type cpx22 --image ubuntu-24.04 \
    --location fsn1 --ssh-key <your-key>
done
hcloud server list
```

## Run

```sh
scripts/real-host-acceptance.sh <core-ip> <edge-ip>
scripts/cli-smoke-test.sh       <core-ip> <edge-ip>   # optional, same pair
```

`real-host-acceptance.sh` exits non-zero if any route check does not return
`200`; a green run ends with `ACCEPTANCE PASSED` and lines like:

```
[..] TIMING machine-init=53s
[..] TIMING machine-add=40s
[..] TIMING deploy=13s
[..]   gateway <core> -> HTTP 200
[..]   gateway <edge> -> HTTP 200
[..]   post-restart route -> HTTP 200
[..] ACCEPTANCE PASSED
```

`cli-smoke-test.sh` prints a `$ ployz <cmd>` / output / `[exit N]` block per
command; redirect it to a file to keep the transcript.

## Tear down

The scripts never delete hosts — do it yourself so idle boxes stop billing:

```sh
vultr-cli instance list | awk '/ployz-/{print $1}' | xargs -n1 vultr-cli instance delete
# or: hcloud server delete ployz-core ployz-edge
```
