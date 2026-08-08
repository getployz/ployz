# Contributor Code Map

This is the starting point for changing Ployz. Product language is canonical in
[`CONTEXT.md`](../../CONTEXT.md), product direction in
[`VISION.md`](../../VISION.md), and accepted architecture in
[`docs/adr/`](../adr/).

**v2 is coreless.** [ADR 0040](../adr/0040-corrosion-replaces-the-core-and-nats.md)
replaces the incumbent core, sequencer, and NATS transport with Corrosion rows
and HTTP/JSON/SSE over the mesh. The workspace collapse establishes the final
crate boundaries before the Corrosion, API, mesh, Keeper, and command slices
populate them; a present crate or role shell does not imply that every v2 slice
is implemented yet.

## Runtime shape

Ployz ships one `ployzd` artifact. Systemd supervises separate invocations:

| Role | Owns | Privilege |
| --- | --- | --- |
| Keeper | machine substrate convergence: mesh, eBPF, sysctls, firewall, component swaps | root with the required host capabilities |
| API | HTTP/JSON commands, SSE watches, Docker and imperative operations | unprivileged; Docker socket access is root-equivalent |
| Gateway | public ingress from route and container rows | unprivileged with ambient bind capability for 80/443 |
| DNS | machine-local service resolution from container rows | unprivileged with ambient bind capability for 53 |

Stock Docker and an exact-pinned Corrosion sidecar are separately supervised.
`ployzd` is not a supervisor. Keeper is mandatory machine substrate rather than
an operator-selectable role.

## State and legal read paths

Classify state before choosing a module or store:

| Kind | Owner | Legal read path |
| --- | --- | --- |
| Operator decision | the operator command stream owns one Corrosion config row | typed row readers in `ployz-core`; the API fold validates and writes through the bounded Corrosion client |
| Machine testimony | exactly one machine owns its status row | typed row readers; freshness stays visible and is never promoted into authority |
| Wake signal | nobody owns truth in a subscription notification | re-query the scoped rows; a notification is invalidation, never an authoritative delta |
| Operation summary | the command driver owns at most three Corrosion summary rows | typed operation queries from any machine |
| Operation detail | the command driver owns machine-local JSONL | replay/follow from that driver over SSE; logs are evidence, not cluster truth |
| Execution reality | Docker or machine-local substrate owns the fact | observe at the point of use; rows report reality but do not replace it |

The one-authority-per-row law and tolerant reader rules live in the row-model
spec. Do not add a second store or a hidden background writer.

## Production dependency direction

Six product crates remain:

```text
ployz-core
  ^
  +-- ployz-build-executor
  +-- ployz-host-runner
  +-- ployzd
  +-- ployz

ployz-telemetry ----> executable surfaces only
```

The diagram omits direct edges that repeat the same inward dependency. Core is
the compiler-checked domain and wire contract. Build Executor and Host Runner
own process-neutral local mechanics. `ployzd` and `ployz` are downstream
executable surfaces. Telemetry contains no product policy.

Test-only crates under [`testing/`](../../testing/) may depend outward for
fixtures and black-box exercise. Production crates must not depend on testing.
The eBPF crates keep their separate shared-contract, userspace-control, and
program-workspace shape.

## Repository boundaries

### Core contract

[`crates/ployz-core/`](../../crates/ployz-core/) owns typed ids, Corrosion row
documents, HTTP request/reply and SSE event shapes, operation states, and domain
policy shared by callers and responders. TypeScript derives and the
`export-typescript` bin live here behind the `ts` feature.

Core does not own process wiring, concrete HTTP servers or clients, Corrosion
process supervision, Docker clients, filesystem adapters, or CLI presentation.

### Daemon roles

[`crates/ployzd/`](../../crates/ployzd/) owns one shipped binary and the Keeper,
API, Gateway, and DNS process implementations. Keep role entrypoints small.
Transport adapters may call domain policy; domain policy must not import role
wiring.

The shared concrete Corrosion exec/query/subscribe client lives under
`crates/ployzd/src/corrosion/`; every daemon role uses that one adapter. Core
owns its transport-neutral wire shapes and row-reader policy, not the HTTP
client.

Gateway/DNS/certificate mechanics that do not depend on a process transport can
remain local modules until churn proves a crate boundary pays for itself. Do not
split per-role crates ahead of that pain.

### CLI

[`crates/ployz/`](../../crates/ployz/) owns command parsing, the mesh-aware
HTTP/SSE client, execution, and presentation. A CLI refusal names the primitive
that resolves it and performs no work itself.

### Shared build executor

[`crates/ployz-build-executor/`](../../crates/ployz-build-executor/) owns
Dockerfile and Railpack execution mechanics: bounded workspaces, pinned
toolchains, redacted logs, cleanup, and validated OCI layouts. Callers own
admission, operation evidence, and image distribution.

### Host Runner

[`crates/ployz-host-runner/`](../../crates/ployz-host-runner/) owns privileged,
machine-local imperative effects: host profile detection, bounded commands,
artifact staging, supervisor units, firewall work, and ZFS mechanics. Keeper and
explicit API operations compose these effects. Host Runner never decides
cluster truth.

### Telemetry

[`crates/ployz-telemetry/`](../../crates/ployz-telemetry/) owns process-neutral
telemetry adapters. It must not become a domain or orchestration dependency.

### eBPF and testing

[`ebpf/common/`](../../ebpf/common/) owns the shared userspace/program contract,
[`ebpf/control/`](../../ebpf/control/) owns the userspace controller, and
[`ebpf/program/`](../../ebpf/program/) is the separately built eBPF workspace.

[`testing/ployz-e2e/`](../../testing/ployz-e2e/) owns black-box and DinD harness
code. [`testing/corrosion-spike/`](../../testing/corrosion-spike/) is the manual
upstream Corrosion certification harness, not a workspace crate.

## Where does this change go?

| Change | Start here | Keep out |
| --- | --- | --- |
| Row, HTTP DTO, typed refusal, id, invariant, or transition | `crates/ployz-core/` | daemon handles and CLI copy |
| Corrosion query/exec/subscribe adapter | `crates/ployzd/src/corrosion/`, with wire DTOs and reader policy in Core | role-private convenience types |
| Keeper/API/Gateway/DNS behavior | matching module under `crates/ployzd/` | another role's private implementation |
| Dockerfile or Railpack execution mechanic | `crates/ployz-build-executor/` | admission and operation evidence |
| CLI command, mesh dial, HTTP client, or presentation | `crates/ployz/` | Core presentation logic |
| Privileged host effect or supervisor rendering | `crates/ployz-host-runner/` | cluster policy and row ownership |
| Public SDK type | Rust definition in Core, then regenerate `packages/ployz-sdk/` | a second hand-written wire twin |
| eBPF contract/program/controller | `ebpf/{common,program,control}` respectively | daemon-only duplicated layouts |
| Cross-process or cross-machine behavior | a real public seam plus integration coverage | an in-process mock that claims transport proof |

## Test and verification map

Use the lowest level that proves the changed seam, then run the final gates in
[`AGENTS.md`](../../AGENTS.md).

| Level | Location | Use it when |
| --- | --- | --- |
| Colocated unit tests | beside code in `crates/*/src/` | pure policy, parsers, renderers, transitions, and local mechanics |
| Crate integration tests | `crates/*/tests/` | a public crate boundary needs exercise without process wiring |
| Daemon/CLI integration | `crates/ployzd/tests/`, `crates/ployz/tests/` | role or command behavior crosses internal modules |
| Black-box cluster E2E | `testing/ployz-e2e/` | shipped binaries or multiple processes must be observed only through public surfaces |
| Docker-in-Docker harness | `testing/ployz-e2e/`, `scripts/dind-e2e.sh` | compile and test the role-neutral harness now; add a black-box scenario with the v2 public seam that needs it |
| Real-host certification | not yet restored for v2 | add a harness with the install, WireGuard/tcx, firewall, or public DNS/TLS slice whose behavior it proves |

Tests of a deleted incumbent behavior are deleted with their subject. A v2
slice restores coverage at the new public seam; it does not keep compatibility
tests for a transport or authority model that no longer exists.
