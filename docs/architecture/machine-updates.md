# Machine Updates

Ployz v1 updates machine substrate through explicit single-machine operations.
There is no background update loop, no `latest` channel, no multi-machine
rollout policy, and no cancellation support.

The design goal is the simplest reliable update system that is secure,
idiomatic, and easy to manage. Keeper does local execution; NATS operations own
authority and evidence.

## Commands

The core operation surfaces are:

```text
keeper update <machine> --version <version>
substrate update <machine> --version <version>
```

`machine update <machine> --version <version>` may exist as CLI convenience. It
runs keeper update first, waits for completion, then runs substrate update. Core
still records two bounded operations.

Every update targets exactly one machine and one exact Ployz version. Version
ranges, channels, and `latest` are rejected.

## Keeper

Keeper is the long-running machine-local executor for updateable substrate. It
has its own machine-scoped NATS credentials and exposes machine-scoped service
endpoints. Keeper does not choose versions and does not mutate cluster truth.

Keeper update is separate from substrate update because keeper is the executor
for substrate steps. A keeper update stages and verifies the requested keeper
artifact, records keeper handoff state locally, restarts the keeper service, and
the new keeper resumes the same operation to report the terminal result.

Substrate update requires keeper to already be at the requested Ployz version.
If the current keeper version is known and does not match, the API rejects the
substrate update before creating the operation.

## Release Source

A machine has one configured release source from machine bootstrap. Update
operations read that release source to resolve the requested exact Ployz version
into artifact metadata. Updates do not change the release source.

## Substrate State

Keeper uses assigned substrate state stored locally on the machine to decide
which components are relevant. That state guides local update execution but is
not cluster truth.

Relevant components depend on the machine:

- every machine has keeper and assigned `ployzd` roles,
- only core machines have `nats-server`,
- only gateway machines have gateway,
- only DNS machines have DNS,
- only dataplane-enabled machines have eBPF.

## Substrate Update Flow

Substrate update uses idempotent check/apply steps:

```text
resolve requested version
load assigned substrate state
compute relevant non-keeper components
stage and verify all relevant artifacts
run substrate preflight for every relevant component
if any preflight fails, stop before activation
activate relevant components in order
stop on first activation failure
complete or fail with evidence
```

Already-in-sync components are successful no-ops with evidence.

The machine substrate lock serializes keeper update, substrate update, bootstrap
finalization, role assignment changes, and future release-source changes for a
machine. If the lock is busy, the API returns resource busy and does not create
an operation.

## Activation Strategies

Each component has a component-specific activation strategy:

```text
ployzd      -> bounded restart
nats-server -> lame-duck restart + health check
gateway     -> graceful gateway upgrade
dns         -> graceful DNS upgrade
ebpf        -> eBPF link replacement
```

Bounded restart stages and verifies the new artifact, atomically points the
unit at the staged version, restarts the affected service, waits for a bounded
health result, and records the observed result.

NATS server activation uses NATS lame duck mode before restart, then keeper
reconnects and waits for NATS health before reporting completion. If health
fails, keeper records failure evidence.

Gateway activation uses the gateway's graceful upgrade support. Keeper stages
the new gateway artifact, starts the new gateway in upgrade mode, signals the
old gateway to transfer sockets, verifies the new gateway is serving, and lets
the old gateway drain within its grace period.

DNS activation uses the DNS role's graceful upgrade support. Keeper stages the
new DNS artifact, starts the new DNS process in upgrade mode, verifies that it
can load current or last-known-good serving state, verifies local UDP and TCP
answers, transfers or overlaps listener ownership, and lets the old DNS process
drain within its grace period. The listener should remain available throughout
the activation path; if the new process cannot serve the expected answers,
keeper leaves the old process serving and records failure evidence.

eBPF activation is not file replacement. Keeper uses the eBPF controller to
verify/load the new program, check pinned map compatibility, update the attached
link/program where supported, verify dataplane observations, and only then
detach the old program.

## Out Of Scope

V1 does not include:

- automatic updates,
- multi-machine rollout,
- channels or `latest`,
- cancellation,
- automatic rollback,
- tunnel updates,
- changing the release source during update.
