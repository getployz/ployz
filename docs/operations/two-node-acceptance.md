# Disposable Two-Node Product Proof On Hetzner

This runbook proves Ployz on real machines. Hetzner is not part of the product
shape; it only gives us two fresh Linux hosts, public IPs, SSH, and cleanup.

The rule is blunt: if code would still matter after replacing Hetzner with a
homelab box or another VPS provider, it belongs in Ployz. If it only exists to
create or delete Hetzner servers, it stays in this shell harness.

The proof bar is:

- create two fresh Hetzner machines,
- prove SSH is ready,
- install Ployz with the same commands users run,
- add the second machine,
- deploy a real smoke service,
- hit the smoke service through ingress,
- destroy the machines.

## Disposable Host Setup

The script provisions two machines, proves SSH readiness, runs real product
commands, prints product diagnostics, and cleans up. The Hetzner parts should
stay provider glue. Do not add Hetzner-specific Rust code unless the actual
product needs it. Do not add Hetzner-specific operation states, readiness
models, or provider abstractions.

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

Create two machines and run the proof:

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

By default, a successful run deletes both machines. To keep them for product
install testing:

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

The disposable host setup proves:

- explicit Hetzner token and SSH key inputs,
- deterministic server names,
- deterministic cleanup labels,
- two server creates,
- SSH readiness on both machines,
- teardown by cleanup label selector.

Ployz product commands and operation state then prove:

- first-node install,
- second-node add/join,
- NATS over iroh,
- WireGuard/eBPF data plane,
- deploy placement,
- gateway routing.

Anything that only exists because the machines came from Hetzner stays in the
script. Anything that should work on a user VPS, homelab server, or another
cloud must be expressed through normal Ployz commands and operation events.

The script must not grow a second orchestration model. It should run product
commands, wait for visible operation results, print useful diagnostics on
failure, and clean up the hosts.

## First-Node Install Contract

`ployzctl init --node <id>` is the product surface for the first machine. It
must install the same supervised shape locally that the Hetzner proof later
exercises remotely:

- `nats-server`
- `ployzd tunnel --side core`
- `ployzd control`
- `ployzd node --id <id>`
- optional `ployzd gateway`

Keeper owns the local step plan for this install. Hetzner glue should only
create the host and call the product command.

## Machine Add Contract

`ployzctl machine add --name <node>` is the product surface for joining a
second node. The command accepts a machine-add operation and returns bootstrap
material for exactly one joining node.

The pending machine is not schedulable. It can become active only after all
three readiness facts are present:

- NATS tunnel over iroh is connected,
- heartbeat is visible,
- node inspect succeeds.

A reused, invalid, or expired join token fails visibly and preserves operation
evidence. A readiness failure also fails the operation instead of silently
activating the machine.

The joined node process shape is:

- `ployzd tunnel --side edge`
- `ployzd node --id <id>`
- optional `ployzd gateway`
