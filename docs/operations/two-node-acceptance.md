# Disposable Product Smoke Proof

This runbook proves Ployz on real machines. Hetzner is not part of the product
shape; it only gives us two fresh Linux hosts, public IPs, SSH, and cleanup.

The rule is blunt: if code would still matter after replacing Hetzner with a
homelab box or another VPS provider, it belongs in Ployz. If it only exists to
create or delete Hetzner servers, it stays in this shell harness.

The proof bar is:

- create two fresh Hetzner machines,
- prove SSH is ready,
- run the real Ployz install commands,
- run the real Ployz product commands,
- hit one smoke service through ingress,
- destroy the machines.

That is the whole job. Hetzner is not a feature area; it is a disposable host
source for proving the install path and actual product substrate on clean
Linux.

## Harness Boundary

The script provisions two machines, proves SSH readiness, runs real product
commands, stores command output on failure, and cleans up.

Hetzner-specific behavior stops at:

- create server,
- tag server for cleanup,
- wait for SSH,
- run shell commands,
- destroy server.

Everything after SSH is ready must be normal Ployz install/product behavior.
Do not add Hetzner-specific Rust code, provider abstractions, provider
readiness models, provider operation states, retries, recovery, or
provider-aware install policy.

The harness waits for product operation results. It must not learn Ployz
internals. If the script needs more than "run product command, wait for product
result, print product evidence," the product command needs a clearer primitive.

## Disposable Host Setup

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

Ployz product commands and operation state then prove only the minimum real
host path:

- first-node install,
- second-node add/join,
- NATS over iroh,
- WireGuard/eBPF data plane,
- deploy execution,
- gateway routing.

Anything that only exists because the machines came from Hetzner stays in the
script. Anything that should work on a user VPS, homelab server, or another
cloud must be expressed through normal Ployz commands and operation events.

The script must not grow a second orchestration model. It should run product
commands, wait for visible operation results, print the failing command output,
print cleanup instructions, and clean up the hosts.

The product smoke sequence is fixed and small:

```text
ployzctl init --node core-1 ...
ployzctl machine add --name edge-2 ...
ployzctl deploy ...
ployzctl ops watch ...
curl ...
```

Do not add Hetzner-specific variants of those commands.

## First-Node Install Contract

`ployzctl init --node <id>` is the product surface for the first machine. It
must install the same supervised shape locally that the Hetzner proof later
exercises remotely:

- `nats-server`
- `ployzd tunnel --side core`
- `ployzd control`
- `ployzd node --id <id>`
- optional `ployzd gateway`

Keeper owns the local step plan for this install. Hetzner glue only creates
the host and calls the product command.

## Machine Add Contract

`ployzctl machine add --name <node>` is the product surface for joining a
second node. The command accepts a machine-add operation and returns bootstrap
material for exactly one joining node.

The pending machine is not schedulable until the product operation says it is
active. The operation should base that on boring facts:

- NATS tunnel over iroh is connected,
- heartbeat is visible,
- node inspect succeeds.

Those are product facts, not Hetzner facts. The harness should not duplicate
them. A reused, invalid, or expired join token fails visibly and preserves
operation evidence. A readiness failure also fails the operation instead of
silently activating the machine.

The joined node process shape is:

- `ployzd tunnel --side edge`
- `ployzd node --id <id>`
- optional `ployzd gateway`
