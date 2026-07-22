# Contributor Code Map

This is the starting point for changing Ployz. It maps runtime ownership,
state ownership, dependency direction, feature homes, and verification levels
to the repository's current paths. Product language remains canonical in
[`CONTEXT.md`](../../CONTEXT.md), product direction in
[`VISION.md`](../../VISION.md), and architecture decisions in
[`docs/adr/`](../adr/).

## Runtime shape

Ployz ships one `ployzd` artifact, but it does not run every responsibility in
one process. Host Runner installs and supervises separate invocations selected
by role arguments:

| Process | Owns | Implementation |
| --- | --- | --- |
| Control | operator API, operation admission and sequencing, intent and operation evidence, projections, bounded controllers | [`crates/ployzd/src/control/`](../../crates/ployzd/src/control/) |
| Machine | machine RPC, Docker and host effects, machine facts, local dataplane projection | [`crates/ployzd/src/roles/machine/`](../../crates/ployzd/src/roles/machine/) |
| Gateway | ingress process, route projection, certificate serving, gateway testimony | [`crates/ployzd/src/roles/gateway/`](../../crates/ployzd/src/roles/gateway/) |
| DNS | internal resolver process, intent/fact projection, DNS testimony | [`crates/ployzd/src/roles/dns/`](../../crates/ployzd/src/roles/dns/) |

[`crates/ployzd/src/dispatch.rs`](../../crates/ployzd/src/dispatch.rs) selects
one role implementation per process. Shared process lifecycle belongs in
[`crates/ployzd/src/process_support.rs`](../../crates/ployzd/src/process_support.rs),
not in a role module.

The default process sets are product policy in
[`crates/ployz-core/src/machine/roles.rs`](../../crates/ployz-core/src/machine/roles.rs):

- A first machine supervises `nats-server` and runs Control, Machine, Gateway,
  and DNS processes.
- A joined machine runs Machine, Gateway, and DNS processes; it does not run
  Control or its own `nats-server`.
- `--no-gateway` removes Gateway from either set. DNS remains required and
  Machine remains the local execution role.

Workloads, Gateway, DNS, and `nats-server` are independently supervised. A
Control process stopping must not stop the data plane. A Machine process
stopping removes RPC and fresh observations from that machine; it does not
implicitly stop already-running workloads.

## State and legal read paths

Classify a new state value before choosing a module or store. There are four
control-plane kinds:

| Kind | Owner and storage | Legal read path |
| --- | --- | --- |
| Durable operator decision | Control owns core-local intent evidence: roster, lifecycle, route bindings, serving targets, authorized users | Call the single `intent.get` NATS service. `intent.changed` only invalidates; readers do not import Control storage. Domain contracts live in [`crates/ployz-core/src/intent/`](../../crates/ployz-core/src/intent/), storage and service code in [`crates/ployzd/src/control/intent/`](../../crates/ployzd/src/control/intent/). |
| Live machine or role testimony | The responding Machine, Gateway, or DNS process owns the current observation; machine facts derive from Docker and the machine-local fact ledger | Make a bounded NATS service request at the point of use. Build the candidate set from intent and record non-response as typed silence; service discovery is not membership. Contracts live under [`crates/ployz-core/src/machine/`](../../crates/ployz-core/src/machine/) and [`crates/ployz-core/src/network/`](../../crates/ployz-core/src/network/). |
| Fanout invalidation | No subscriber owns truth from the message. Plain NATS carries `intent.changed` and live operation progress | On receipt, re-read the authoritative service or evidence. Repair missed messages with reconnect re-listing or periodic rebroadcast; never persist a delta as authority. Concrete subjects live in [`crates/ployz-nats/src/subjects.rs`](../../crates/ployz-nats/src/subjects.rs). |
| Durable operation evidence | The Control sequencer owns the core-local append-only operation log and status records; an external subscriber may retain history | Read through operation status/list/watch services. Operation models live in [`crates/ployz-core/src/operation/`](../../crates/ployz-core/src/operation/), admission in [`crates/ployzd/src/control/sequencer/`](../../crates/ployzd/src/control/sequencer/), and persistence in [`crates/ployzd/src/control/operation_evidence/`](../../crates/ployzd/src/control/operation_evidence/). |

Docker remains execution reality. Docker labels and the machine-local ledger
are machine-owned recovery evidence, never cluster intent. NATS services do not
become authoritative merely because a responder exists.

## Production dependency direction

Cargo dependencies point inward from executable surfaces and adapters toward
domain and wire contracts:

```text
ployz ----------------> host-runner ----> ployz-nats ----> ployz-sdk-types ----> ployz-core
  |                           |                 |                  |
  +---------------------------+-----------------+------------------+
  +-------------> ployz-build-executor ------> ployz-nats + ployz-core

ployzd -------------------------------> ployz-nats + ployz-sdk-types + ployz-core
  +-----------------------------------> ployz-build-executor
  +-----------------------------------> ployz-ebpf-common (validation contract)

ployz-ebpf-ctl ------------------------> ployz-ebpf-common
ployz-ebpf program --------------------> ployz-ebpf-common
```

The diagram omits repeated direct edges for readability: SDK types depend on
Core; NATS depends on Core and SDK types; Host Runner depends on all three;
`ployz` depends on Host Runner, NATS, SDK types, Core, and telemetry; `ployzd`
depends on NATS, SDK types, Core, telemetry, the shared build executor, and the
shared eBPF validation contract. `ployz-build-executor` depends inward on NATS
for exact log publication and on Core for build contracts; both `ployz` and
`ployzd` use it without depending on each other's process wiring.
[`crates/ployz-telemetry/`](../../crates/ployz-telemetry/) is an
independent adapter used by executable surfaces. Test-only crates under
[`testing/`](../../testing/) may depend outward for fixtures and black-box
exercise; production crates must not depend on them except as dev-dependencies.

Do not introduce reverse dependencies from Core into NATS, daemon wiring, CLI,
Host Runner, SDK generation, eBPF control, or test support.

## Repository boundaries

### Core domain

[`crates/ployz-core/src/`](../../crates/ployz-core/src/) owns typed product
contracts and policy. Its canonical roots are:

- [`operation/`](../../crates/ployz-core/src/operation/) for operation states,
  transitions, events, failures, and projections;
- [`intent/`](../../crates/ployz-core/src/intent/) for durable operator-decision
  models and recovery snapshots;
- [`machine/`](../../crates/ployz-core/src/machine/) for machine identity,
  lifecycle, roles, RPC contracts, runtime facts, and testimony;
- [`network/`](../../crates/ployz-core/src/network/) for dataplane, internal
  DNS, reachability, repair, and status contracts;
- [`certificate/`](../../crates/ployz-core/src/certificate/) for certificate
  and managed-lease contracts;
- [`deploy/`](../../crates/ployz-core/src/deploy/) for deploy input,
  normalization, planning, revisions, routes, and runtime models;
- [`install/`](../../crates/ployz-core/src/install/) for bootstrap and install
  material, paths, roles, and validation.

Core does not own concrete NATS subjects, process wiring, filesystem adapters,
Docker clients, CLI rendering, or generated SDK transport code.

### NATS adapter

[`crates/ployz-nats/src/`](../../crates/ployz-nats/src/) owns TLS connection
setup, concrete subjects and endpoints, permissions, NATS server configuration,
service protocol/runtime helpers, typed operation clients, and TypeScript NATS
metadata. Wire naming has one implementation here.

### Daemon roles

[`crates/ployzd/src/`](../../crates/ployzd/src/) is process wiring and runtime
implementation. Put Control behavior under `control/`, local execution under
`roles/machine/`, ingress under `roles/gateway/`, resolver behavior under
`roles/dns/`, and certificate execution shared by Control/Gateway under
`certificate/`. Cross-role calls use NATS contracts rather than importing
another role's private implementation.

### Shared build executor

[`crates/ployz-build-executor/src/`](../../crates/ployz-build-executor/src/)
owns the process-wiring-neutral Dockerfile and Railpack execution engine: native
runtime readiness, pinned toolchain lowering, bounded workspace lifecycle,
redacted build logs, and validated OCI-layout output. Machine-role and external
CLI runtimes own admission, NATS endpoints, image ingestion or push, and
terminal operation evidence; they call this crate rather than duplicating its
Docker mechanics. The shared engine depends only on Core build contracts and
the NATS log adapter, never on `ployz` or `ployzd` process wiring.

### CLI features

[`crates/ployz/src/`](../../crates/ployz/src/) is organized by user-facing
feature: `deploy/`, `machine/`, `operation/`, `network/`, `certificate/`,
`ingress/`, `namespace/`, `service/`, `volume/`, and `logs/`. Each feature owns
its command parsing, execution, and presentation. Shared client/context and
output mechanics belong in `execution_support/`; product policy does not.

### Host Runner

[`crates/ployz-host-runner/src/`](../../crates/ployz-host-runner/src/) owns
machine-local privileged planning and execution. `plan/` creates typed steps,
`execution/` applies them, `lifecycle/` owns bootstrap/join/update flows, and
`recovery/` owns promote/demote. Host Runner may mutate local substrate; it
does not choose cluster policy or mutate cluster truth without an operation.

### SDK contracts

[`crates/ployz-sdk-types/src/`](../../crates/ployz-sdk-types/src/) is the public
schema and operation-contract registry. The generated TypeScript package is
[`packages/ployz-sdk/`](../../packages/ployz-sdk/). Change domain wire shapes in
Core or SDK types, keep NATS metadata in `ployz-nats`, then regenerate and
commit real output drift.

### eBPF

[`ebpf/common/`](../../ebpf/common/) owns the shared userspace/program contract,
[`ebpf/control/`](../../ebpf/control/) owns the userspace controller, and
[`ebpf/program/`](../../ebpf/program/) is the separately built eBPF program
workspace. Machine-role host dataplane integration lives in
[`crates/ployzd/src/roles/machine/execution/host_dataplane/`](../../crates/ployzd/src/roles/machine/execution/host_dataplane/).

### Testing support

[`testing/ployz-test-support/`](../../testing/ployz-test-support/) owns shared
fixtures and helpers, [`testing/ployz-test-lease-worker/`](../../testing/ployz-test-lease-worker/)
owns the fake external lease service, and
[`testing/ployz-e2e/`](../../testing/ployz-e2e/) owns black-box cluster and DinD
scenarios. None is a production abstraction.

## Where does this change go?

| Change | Start here | Keep out |
| --- | --- | --- |
| New domain model, invariant, transition, or typed failure | The owning canonical module in `crates/ployz-core/src/` | NATS strings, daemon handles, CLI copy |
| NATS subject, endpoint, permission, connection, or service transport | `crates/ployz-nats/src/` | Core policy and orchestration convenience |
| Control operation admission, evidence, intent, or projection | The matching directory under `crates/ployzd/src/control/` | Role-private runtime effects |
| Machine, Gateway, or DNS behavior | `crates/ployzd/src/roles/machine/`, `crates/ployzd/src/roles/gateway/`, or `crates/ployzd/src/roles/dns/` | Another role's private module; call its service instead |
| Shared Dockerfile or Railpack build execution mechanic | `crates/ployz-build-executor/src/` | CLI or daemon admission, NATS service ownership, image distribution, operation evidence |
| CLI command or presentation | Matching feature under `crates/ployz/src/` | Core and transport presentation logic |
| Public SDK request, response, or operation contract | `crates/ployz-sdk-types/src/`, then generated output in `packages/ployz-sdk/` | Daemon-only implementation types |
| Privileged install, bootstrap, update, recovery, or supervisor effect | `crates/ployz-host-runner/src/`; `scripts/ployz.sh` only for explicit public release delivery | Cluster policy decisions |
| Machine-local Docker, network, WireGuard, eBPF, filesystem, or process effect | Machine execution adapters under `crates/ployzd/src/roles/machine/execution/`, or Host Runner when it is substrate lifecycle | Core intent and shared truth |
| Cross-process or cross-machine behavior | A real service seam plus integration coverage; use `testing/ployz-e2e/` when binaries or machines must be black-box | In-process mocks that claim to prove transport/process behavior |

## Test and verification map

Use the lowest level that can reliably prove the changed seam, then run the
standard final gates.

| Level | Location | Use it when |
| --- | --- | --- |
| Colocated private tests | `#[cfg(test)]` modules beside code throughout `crates/*/src/` | Pure policy, state transitions, parsers, renderers, and private module invariants can be exercised deterministically. |
| Crate integration tests | `crates/*/tests/` | A crate's public surface or adapter boundary needs exercise without private access. |
| Daemon role integration | [`crates/ployzd/src/tests/`](../../crates/ployzd/src/tests/) and [`crates/ployzd/tests/`](../../crates/ployzd/tests/) | Control, Machine, Gateway, DNS, process lifecycle, SQLite, or NATS seams need realistic in-process/process-level composition on one host. |
| Black-box CLI and cluster E2E | [`crates/ployz/tests/`](../../crates/ployz/tests/) and [`testing/ployz-e2e/`](../../testing/ployz-e2e/) | Command contracts, shipped binaries, or multi-component behavior must be observed only through public surfaces. |
| Docker-in-Docker | [`scripts/dind-e2e.sh`](../../scripts/dind-e2e.sh) and [`docs/operations/dind-e2e.md`](../operations/dind-e2e.md) | Real cross-process or cross-machine Docker execution, supervision, bootstrap/install, network namespaces, gateway/TLS/DNS traffic, or credential enforcement cannot be covered deterministically below this level. Run the full suite once on the sealed candidate when applicable. |
| Real-host certification | [`scripts/real-host-acceptance.sh`](../../scripts/real-host-acceptance.sh), [`scripts/cli-smoke-test.sh`](../../scripts/cli-smoke-test.sh), and [`docs/operations/real-host-acceptance.md`](../operations/real-host-acceptance.md) | The public install path, real tcx eBPF, real WireGuard, host firewalls, public DNS/TLS, or mixed architectures are the behavior under test. |

Every final candidate runs Rust formatting, workspace Clippy, workspace tests,
and `pnpm check:generated` from `packages/ployz-sdk`. Run SDK typecheck/tests
when SDK source or generated output changes. DinD is not a default tax: record
the deterministic covering tests when it is not applicable, or name the real
seam that requires it. Documentation-only changes record SDK typecheck/tests
and DinD as not applicable.

For the complete gate commands and scheduling rules, follow
[`AGENTS.md`](../../AGENTS.md). For the quick local release loop, see
[`docs/operations/fast-dev-loop.md`](../operations/fast-dev-loop.md).
