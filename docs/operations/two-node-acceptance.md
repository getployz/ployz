# Disposable Product Smoke Proof

This runbook proves that Ployz installs and runs on two fresh Linux machines.
Hetzner only supplies disposable hosts, public IPs, SSH, and cleanup.

> **Local acceptance path:** the same two-machine product flow (first-node
> install, machine add/join, deploy, gateway routing) runs locally as gated
> Rust tests in the Docker-in-Docker harness — `scripts/dind-e2e.sh`, see
> [`dind-e2e.md`](dind-e2e.md). Use the DinD harness for day-to-day
> acceptance; this Hetzner runbook remains the real-host proof.

The rule is blunt: Hetzner is not an architecture slice. If the product cannot
install, join a node, deploy, route traffic, and expose operation status through
normal Ployz commands, this proof fails.

The proof bar is one disposable command:

- create two fresh Hetzner machines,
- prove SSH is ready,
- run the real Ployz install commands,
- run the real Ployz product commands,
- hit one smoke service through ingress,
- destroy the machines.

That is the whole job. If install, join, deploy, routing, operation status,
direct TLS NATS connectivity, or the eBPF/WireGuard data plane needs extra
help, fix Ployz.
Do not make the Hetzner harness smarter.

## Harness Boundary

The script provisions two machines, proves SSH readiness, runs real product
commands, stores command output on failure, and cleans up.

Hetzner-specific behavior stops at:

- create server,
- tag server for cleanup,
- wait for SSH,
- stage the selected artifacts,
- run product shell commands,
- capture command output,
- destroy server.

Everything after SSH is ready is normal Ployz install/product behavior. Do not
add Hetzner-specific Rust code, provider abstractions, provider readiness
models, provider operation states, provider diagnostics, retries, recovery, or
provider-aware install policy. The harness may wait for SSH and command
completion; product readiness comes from product commands and operation output.

## Disposable Host Setup

Required tools:

- `hcloud`
- `jq`
- `ssh`

The script uses the official Hetzner Cloud CLI:
<https://github.com/hetznercloud/cli>

Authentication can use either `HCLOUD_TOKEN` or an active `hcloud` context.

Required environment:

```sh
export HETZNER_SSH_KEY=ployz-ci
export PLOYZ_SSH_PRIVATE_KEY="$HOME/.ssh/ployz-ci"
```

`HETZNER_SSH_KEY` is the Hetzner Cloud SSH key id or name attached to new
servers. `PLOYZ_SSH_PRIVATE_KEY` is the matching local private key used to
prove SSH readiness.

Prepare local artifacts:

```sh
scripts/prepare-h0-artifacts.sh
```

The prep command builds Linux amd64 release Ployz binaries into
`/tmp/ployz-linux-amd64-target`, builds current source eBPF bytecode into
`/tmp/ployz-rust-ebpf-source-target`, downloads a Linux amd64 `nats-server`
artifact when `PLOYZ_ACCEPTANCE_NATS_SERVER` is not set, validates the eBPF
bytecode, and prints the exports the acceptance script will use.

Optional environment:

```sh
export HETZNER_LOCATION=fsn1
export HETZNER_SERVER_TYPE=cx23
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
scripts/hetzner-two-node-acceptance.sh cleanup --run-id ci-42
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

Ployz product commands and operation state prove the real host path:

- first-node install,
- second-node add/join,
- direct TLS NATS connectivity,
- WireGuard/eBPF data plane,
- deploy execution,
- gateway routing.

Anything that only exists because the machines came from Hetzner stays in the
script. Everything else is product behavior. The script runs commands, prints
the failing command output, prints cleanup instructions, and cleans up hosts.

The product smoke sequence is fixed and small:

```text
ployzctl init --node core-1 ...
ployzctl machine add --name edge-2 ...
ployzctl deploy ...
ployzctl ops watch ...
curl ...
```

Do not add Hetzner-specific variants of those commands.

## Product Expectations

The proof calls the same product surfaces a user would call:

- `ployzctl init --node <id>` installs the first node.
- `ployzctl machine add --name <node>` joins the second node.
- `ployzctl deploy ...` deploys one smoke service.
- `ployzctl ops watch ...` reports operation progress.
- `curl ...` proves one routed request reaches the service.

The harness does not decide whether a node is active, a route is ready, or a
deploy succeeded. Ployz operations decide those things.

## Done

H0 is done when one command creates two fresh hosts, installs Ployz, joins the
second node, deploys one smoke service, gets one successful response through
the product route/data-plane path, and deletes the hosts.

That pass is the only assertion. No provider abstraction, provider-specific
Rust, provider operation state, provider recovery model, or Hetzner diagnostics
are required for v1.
