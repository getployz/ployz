# Real-Host Acceptance

`scripts/real-host-acceptance.sh <core-ip> <edge-ip>` is the release-candidate
capstone that DinD cannot replace: real tcx eBPF, WireGuard, mixed architecture,
host firewalls, public DNS/TLS, and the public installer on two fresh machines.

It proves this flow:

1. Install Ployz and form the cluster on the core.
2. Join the edge.
3. Deploy an image-based Compose app with one replica on each machine.
4. Receive and serve its managed public HTTPS URL.
5. Route through both gateways, including to the replica on the other machine.
6. Restart the control daemon without interrupting the public route.

The fixed matrix deliberately covers both release architectures and both
managed firewall backends:

| Role | OS | Architecture | Firewall |
| --- | --- | --- | --- |
| core | Rocky Linux 9 | amd64 (`x86_64`) | firewalld, already active |
| edge | Ubuntu 24.04 | arm64 (`aarch64`) | UFW, enabled by the script |

Use fresh hosts. The script rejects a different OS/architecture pair, requires
root SSH to both public IPs, and leaves the machines running for inspection.
The operator machine needs Bash, SSH, curl, and its key in `ssh-agent`. At
least 1 GB RAM and 1 vCPU per host is sufficient.

## Before running

Promote the exact build under test to the public alpha channel. The public
installer and `machine init` resolve that channel; this harness does not test
unpublished local binaries. See [`release.md`](release.md).

Provision the two hosts in the same region when possible. Hetzner's `cx22` or
`cpx22` covers the Rocky amd64 core and `cax11` covers the Ubuntu arm64 edge:

```sh
hcloud server create --name ployz-core --type cpx22 --image rocky-9 \
  --location fsn1 --ssh-key <your-key>
hcloud server create --name ployz-edge --type cax11 --image ubuntu-24.04 \
  --location fsn1 --ssh-key <your-key>
hcloud server list
```

If the provider has an external firewall, allow inbound `22/tcp`, `80/tcp`,
`443/tcp`, `4222/tcp`, and `51820/udp` to both hosts. That firewall is outside
the guest and remains operator-owned. Inside the guests, keeper opens exactly
the Ployz ports in firewalld and UFW; the script verifies both runtime and
permanent firewalld rules and the UFW rules. Do not pass
`--host-ports-assured-externally` for this run.

## Run

```sh
scripts/real-host-acceptance.sh <core-ip> <edge-ip>
```

Run it once on fresh hosts. A green run ends with:

```text
[..] gateway <core-ip> -> HTTPS 200
[..] gateway <edge-ip> -> HTTPS 200
[..] post-restart route -> HTTPS 200
[..] ACCEPTANCE PASSED: mixed-arch + firewalld/UFW + public HTTPS
```

The script first checks public DNS/TLS. It then stops each gateway's local
replica in turn and uses `curl --resolve` through that gateway, proving the
request reaches the other machine while TLS still validates the managed
certificate. A continuous probe must see no failed request while the control
daemon restarts. Any failed command or non-200 response exits non-zero. Keep
the full transcript as release evidence. `scripts/cli-smoke-test.sh` is an
optional broader CLI check against the cluster left by this run.

## Tear down

The harness never deletes hosts:

```sh
hcloud server delete ployz-core ployz-edge
```
