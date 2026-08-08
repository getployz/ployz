# Binary and Crate Topology After the Collapse

First-draft spec from the wayfinder ticket [Decide: binary and crate topology
after the collapse](https://github.com/getployz/ployz/issues/790). Governs the
coreless v2 workspace: one `ployzd` binary + Corrosion + Docker, no core, no
sequencer, no NATS. Downstream of [Keeper's charter (#784)](https://github.com/getployz/ployz/issues/784),
[the row model (#785)](https://github.com/getployz/ployz/issues/785), and the
[mesh-provider seam (#787)](https://github.com/getployz/ployz/issues/787).

## The process map

One shipped binary, role subcommands. `ployzd <role>`. Keeper is a role, not a
separate binary: one artifact to stage, hash, and swap; the symlink flip is
atomic across every role at once; there is no keeper↔fold version skew to
reason about; and the `OnFailure=` revert (#784) covers a bad keeper because a
dead new keeper can't take the command that would fix it.

Systemd supervises; `ployzd` is not a supervisor (#784). Keeper is the sole root
process; the folds drop privilege and carry per-unit ambient caps.

```
per-machine systemd units
  base (every machine):
    ployzd-keeper.service   root, NET_ADMIN    WG/eBPF/sysctls/firewall
    ployzd-api.service      unpriv             HTTP/JSON/SSE + docker + deploy
    corrosion.service       unpriv             stock pinned binary (#779)
    docker.service          (stock)
  role-added:
    ployzd-gateway.service  unpriv, cap :80/:443
    ployzd-dns.service      unpriv, cap :53
```

Keeper authors the unit files during a swap; the api fold's Docker socket is
root-equivalent, so the privilege split is blast radius, not a security
boundary (#784).

## The crate map

Five product crates. Boundaries exist to keep the two expensive-to-rebuild
crates cheap: `core` is upstream of everything (a rebuild cascades to all), and
`ployzd` is the downstream tail. A crate boundary shields both from churn in a
concern that isn't theirs. We keep only boundaries that pay for themselves; we
do not split ahead of pain.

```
crates/
  ployz-core          row types + HTTP DTOs + domain policy;
                      the compiler-checked contract shared by CLI + daemon.
                      ts-rs derives gated behind `ts` feature; export bins here.
  ployzd              keeper + api + gateway + dns roles, one binary.
  ployz               CLI: HTTP/JSON/SSE client over the mesh.
  ployz-host-runner   privileged machine-local imperative effects
                      (ZFS, firewall, command exec, identity mint,
                      content-addressed artifact staging, one-shot join).
  ployz-telemetry     sentry/posthog. Unchanged.
ebpf/{common,control,program}   unchanged
testing/ployz-e2e
```

`ployzd` lands at ~35–50k after the collapse (the NATS/sequencer/intent
machinery that bloats today's 111k is exactly what dies), so it stays one crate;
splitting gateway/DNS into their own crates is the named upgrade path if either
fold starts to churn, not a v1 cut. Full per-role crates are YAGNI.

### Why no `ployz-sdk-types` crate

`ts-rs` `#[derive(TS)]` must sit on the type definition (orphan rule). The types
now live in `core`, so the derives must too — a separate crate could hold no
types, only the 80-byte export bin. The `ts-rs` proc-macro is kept out of the
hot build path by a feature gate, not a crate:

```
#[cfg_attr(feature = "ts", derive(TS))]
[features] ts = ["dep:ts-rs"]
```

Normal `ployzd`/`ployz` builds skip `ts-rs`; the export bins run with
`--features ts`.

### Why `ployz-host-runner` stays

After #784's dispositions the crate is not hollow: ~7.6k of coherent privileged
effects survive (ZFS, firewall, command exec, WG/identity mint, artifact
staging, release manifest) plus a slimmed one-shot join path — exactly what root
Keeper's converge loop and the api-fold swap path call into. Folding it into
`ployzd` would move host-effect churn onto the daemon's serial rebuild tail for
no build-time gain (cargo already parallelizes separate crates). The crate needs
its dead modules deleted and a rename to shed core-era vocabulary (#732), but
that is cleanup inside a surviving boundary.

## What `ployz-core` becomes

The compiler-checked contract that replaced the NATS IDL. It owns, in one place
per shape:

- **Row types** — the Corrosion JSON-document shapes and their virtual-column
  keys (#785).
- **HTTP DTOs** — request/reply and SSE event shapes for the api fold.
- **Domain policy** — the operation/state enums, the row-ownership law's typed
  ids, deploy/placement/cert policy that CLI and daemon must agree on.

Row types and HTTP DTOs both carry gated `#[derive(TS)]`; TypeScript generates
from the same definitions the Rust code uses, so there is no wire twin to keep
in sync.

## SDK generation and transport

- **Types**: `ts-rs`, unchanged mechanism. The `export-typescript` bin lives in
  Core behind `--features ts`; `pnpm check:generated` is the diff gate.
- **Runtime transport**: a thin hand-written client — `fetch()` for
  request/reply, `EventSource` for SSE progress. The `@nats-io/transport-node`
  dependency is dropped. No OpenAPI toolchain, no generated client: the wire is
  plain JSON + SSE over the mesh, and generated strict validators would break
  the tolerant-read rule below.

## Cross-version compatibility (Cloud ↔ any-version clusters)

Cloud is one continuously deployed hub; clusters are many responders each frozen
at their installed version. This inverts Stripe's shape (one current service,
many pinned callers): here the always-current thing is the *caller*. The rule
that follows:

> Cloud carries all cross-version adaptation. A cluster exposes a stable,
> additive, self-describing contract and never carries a line of code to satisfy
> a future Cloud.

This concentrates versioning complexity in the one component that is trivially
updatable and keeps clusters — which you cannot reach to patch — frozen-simple.
Four rules, three of them ~free because they reuse decided machinery:

1. **One versioning law for rows and HTTP.** Reuse the row `v` (#785): additive
   within a major, unknown fields tolerated on read (serde/ts-rs default),
   breaking change = a coarse rare major bump. Rows are a Cloud-facing contract
   too (Cloud subscribes as a mesh peer), so both surfaces share one `v` axis —
   never two drifting ones.
2. **Every cluster advertises version + capabilities.** One `GET /version →
   {major, build, features[]}` (the `cluster` row already carries schema `v`).
   Cloud feature-detects and degrades; it never infers capability from behavior.
3. **Cloud holds the down-adapters, keyed by cluster major.** Stripe's
   ordered-transform-modules idea, living in Cloud/SDK instead of the cluster.
   Built only when a second major exists — YAGNI until then.
4. **The generated SDK diff is the tripwire.** `check:generated` regenerates
   the public v2 types: additive change grows them (fine), a
   rename/remove diffs an existing entry and forces a conscious major bump.

Both directions fall out: Cloud-newer-than-cluster forms old-shape requests via
its major down-adapter; Cloud-older-than-cluster relies on tolerant reads plus
the capability gate, and #785's rollout-ordering law ("binaries everywhere
before new shapes") guarantees Cloud ships major support before that major's
clusters exist.

During alpha no compatibility is owed (#788 froze the incumbent dead; coreless
ships `v0.1.0-alpha.N` and may break freely). This policy is a GA obligation.
The move now is only to lay the three cheap seams — the `/version` endpoint, the
single row+HTTP `v` law, the fixture tripwire — so GA is not a retrofit. The
adapter machinery itself is built at major #2.

## The deletion list, sized

Whole crates removed:

```
crates/ployz-nats                  7.2k   NATS conn/services/subjects; export bins move to core
crates/ployz-sdk-types             5.7k   folds into core (~0.6k nats-era modules die outright)
testing/ployz-test-lease-worker    1.0k   NATS lease test
```

Removed or rewritten inside surviving crates:

```
ployzd/control/sequencer           2.3k   no sequencer, no admission (#782/#785)
ployzd/control/intent              2.6k   intent files -> Corrosion rows (#785)
ployzd/control/projection          2.8k   projection -> rows watched live (#785)
ployzd/control/role_client         1.8k   NATS RPC -> HTTP/JSON
ployzd/control/operation_evidence  5.6k   -> coarse operation rows (#888)
ployzd/control/operations         ~21k    sequencer-driven -> imperative ops (heavy rewrite)
ployzd/roles/machine              ~22k    NATS services -> HTTP api fold + keeper (heavy rewrite)
host-runner (dead per #784)        4.8k   founder/cloud bootstrap, promote/demote,
                                          nats_material, substrate_update, cloud_client
host-runner (mixed)               ~6k     machine_join/plan: join survives one-shot, rest dies
```

Outright deleted code (crates + clearly-dead modules): **~24k**. Additional
heavy rewrites of surviving modules (`operations`, `roles/machine`) that shed
their NATS/sequencer skeleton: **~43k** touched. `dns` (1.6k), `gateway`
(5.2k), and `certificate` (2.0k) survive largely intact as folds that watch
Corrosion directly.
