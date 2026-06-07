# Two-Node Product Smoke On Hetzner

This runbook is for proving Ployz on real substrate. Hetzner is not a product
abstraction here; it only gives us two fresh Linux machines, public IPs, SSH,
and cleanup.

The acceptance bar is:

- create two fresh Hetzner machines,
- prove SSH is ready,
- install Ployz with the same commands users run,
- add the second machine,
- deploy a real smoke service,
- prove cross-node networking and ingress,
- destroy the machines.

## Substrate Smoke

The current H0 script only provisions two machines and proves SSH readiness.
Later H-slices should add product commands to this same flow. Do not add
Hetzner-specific Rust code unless the actual product needs it.

Required tools:

- `hcloud`
- `jq`
- `ssh`

The script uses the official Hetzner Cloud CLI:
<https://github.com/hetznercloud/cli>

Required environment:

```sh
export HCLOUD_TOKEN=...
export HETZNER_SSH_KEY=ployz-ci
export PLOYZ_SSH_PRIVATE_KEY="$HOME/.ssh/ployz-ci"
```

`HETZNER_SSH_KEY` is the Hetzner Cloud SSH key id or name attached to new
servers. `PLOYZ_SSH_PRIVATE_KEY` is the matching local private key used to
prove SSH readiness.

Optional environment:

```sh
export HETZNER_LOCATION=fsn1
export HETZNER_SERVER_TYPE=cx22
export HETZNER_IMAGE=ubuntu-24.04
export PLOYZ_SSH_USER=root
export PLOYZ_SSH_READY_TIMEOUT_SECONDS=300
```

Create two machines and wait for SSH:

```sh
scripts/hetzner-two-node-acceptance.sh up --run-id ci-42
```

The harness creates:

```text
ployz-ci-42-core-1
ployz-ci-42-edge-2
```

Every created server carries deterministic cleanup labels:

```text
ployz=acceptance
ployz_run=ci-42
ployz_cleanup=true
```

By default, a successful substrate smoke run deletes both machines after SSH is
proved. To keep them for product install testing:

```sh
PLOYZ_ACCEPTANCE_KEEP=1 scripts/hetzner-two-node-acceptance.sh up --run-id ci-42
```

Cleanup is always label-based:

```sh
HCLOUD_TOKEN=... scripts/hetzner-two-node-acceptance.sh cleanup --run-id ci-42
```

If provisioning or SSH readiness fails, the script attempts cleanup
automatically. If that cleanup fails, it prints the cleanup command.

## Scope

The substrate smoke proves:

- explicit Hetzner token and SSH key inputs,
- deterministic server names,
- deterministic cleanup labels,
- two server creates,
- SSH readiness on both machines,
- teardown by cleanup label selector.

The complete acceptance flow must prove:

- first-node install,
- second-node add/join,
- NATS over iroh,
- WireGuard/eBPF data plane,
- deploy placement,
- gateway routing.

## First-Node Install Contract

`ployzctl init --node <id>` is the product surface for H1. It must install the
same supervised shape locally that the Hetzner proof later exercises remotely:

- `nats-server`
- `ployzd tunnel --side core`
- `ployzd control`
- `ployzd node --id <id>`
- optional `ployzd gateway`

Keeper owns the local step plan for this install. Hetzner glue should only
create the host and call the product command.
