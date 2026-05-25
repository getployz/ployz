---
title: "feat: Build Ployz 1.0 State And Substrate"
type: feat
status: draft
date: 2026-05-24
origin:
  - VISION.md
  - docs/plans/2026-05-24-001-feat-corrosion-store-iroh-membership-plan.md
  - docs/plans/2026-05-24-002-feat-ployz-1-0-cli-shape-and-workflows-plan.md
  - /Users/nick/dev/uncloud/internal/corrosion
  - /Users/nick/dev/uncloud/internal/machine/store/schema.sql
---

# feat: Build Ployz 1.0 State And Substrate

## Summary

Implement the distributed substrate needed by the CLI-first roadmap:
Corrosion-backed row storage, iroh identity/RPC, tickets for bootstrap,
authority island peer networking, namespace rows for deploy grouping, and
daemon runtime adoption.

Keep the boundary blunt:

- `polis` owns distributed substrate primitives.
- `ployz` owns product behavior.
- Ployz adapters in `crates/ployz/src/adapters/polis/` sequence Polis
  primitives into product ports.

Do not build a generic store framework. Copy the `~/dev/uncloud` shape where
it is useful: thin Corrosion client operations, typed row helpers where there
is an actual caller, direct subscriptions, and boring SQL files.

## Design Rules

- Corrosion rows model convergent cluster state, not private in-memory
  snapshots.
- Store one row per independently changing product fact.
- Use explicit primary keys, `NOT NULL` columns, defaults, and ordinary
  indexes.
- Use JSON only for rarely changed opaque metadata or local runtime caches.
- Most rows should have one clear writer: the machine that owns the resource,
  or the command coordinator only for command-owned evidence rows. For
  resource-owned rows, the coordinator RPCs to the owner machine and the owner
  performs the Corrosion write.
- Do not add a generic `operations` table up front. Add command evidence tables
  only where the deploy/volume/branch workflow needs durable replay.
- Do not add claims up front. Owner-machine serialization is the default fence:
  the coordinator RPCs to the machine that owns the row, and that owner
  enforces ordering locally before writing Corrosion. Add an explicit
  distributed claim only when a concrete multi-owner path proves this is not
  enough.
- Corrosion is not the command bus. Peer commands run over bounded iroh RPC.
- Durable peer identity is iroh endpoint ID. Ticket text is a bootstrap
  envelope, not machine truth.

## Boot Order

Corrosion cannot bootstrap iroh because Corrosion starts after iroh.

1. Load local iroh secret key from disk.
2. Start iroh endpoint with local discovery/relay policy.
3. Start internal peer RPC listener over iroh.
4. Join or rejoin through ticket or known endpoint ID if needed.
5. Start local Corrosion agent.
6. Seed Corrosion peers from local config or RPC response.
7. Apply schema files and let Corrosion sync.
8. Start Ployz product services and runtime adoption.

Acceptance test:

- A node restarted with the same local iroh key gets the same endpoint ID,
  rejoins without durable ticket text, and observes existing machine rows.

## Plan Baselines

Preview/apply drift rejection needs concrete evidence. A plan baseline is not a
single timestamp. Each preview that can later be applied records:

- the Corrosion subscription EOQ/change point or query snapshot marker used by
  the coordinator;
- every durable row primary key read by the plan;
- a digest of each row value relevant to the operation;
- live probe receipts with endpoint ID, peer machine ID, operation deadline,
  observed capability/readiness, and probe time;
- operation-specific revalidation rules.

Apply revalidates only the rows and live facts that can make the plan unsafe.
Examples:

- route promotion revalidates route rows, candidate placement, and a fresh
  readiness probe receipt;
- volume fork revalidates source volume owner, source snapshot/watermark, and
  target non-existence with the source owner machine;
- machine remove revalidates that no active placement or volume ownership
  remains.

If the baseline fails, apply stops before mutation and returns a new preview
requirement. This avoids sleeps or vague "wait for the cluster" behavior.

## Core Tables

This is the 1.0 target set. Add tables only when a product slice consumes them.

### `machines`

Owner: the machine itself. A peer can authorize or coordinate a machine add,
but the joining/owning machine writes its own durable machine row once it has
joined the authority island substrate.

```sql
CREATE TABLE machines (
    machine_id TEXT NOT NULL CHECK(length(trim(machine_id)) > 0),
    island_id TEXT NOT NULL CHECK(length(trim(island_id)) > 0),
    name TEXT NOT NULL DEFAULT '',
    iroh_endpoint_id TEXT NOT NULL CHECK(length(trim(iroh_endpoint_id)) > 0),
    wireguard_public_key TEXT NOT NULL CHECK(length(trim(wireguard_public_key)) > 0),
    overlay_ip TEXT NOT NULL CHECK(length(trim(overlay_ip)) > 0),
    capabilities_json TEXT NOT NULL DEFAULT '{}',
    lifecycle TEXT NOT NULL CHECK(lifecycle IN ('active', 'removing', 'tombstoned', 'conflicted', 'deleted')),
    epoch INTEGER NOT NULL CHECK(epoch > 0),
    updated_at INTEGER NOT NULL CHECK(updated_at >= 0),
    PRIMARY KEY (machine_id)
);

CREATE INDEX idx_machines_island ON machines (island_id);
CREATE INDEX idx_machines_lifecycle ON machines (lifecycle);
```

Why `capabilities_json` is acceptable:

- Capabilities are mostly owner-written by one machine.
- They are probe inputs and display data, not independently edited sets by
  multiple writers.
- If a capability becomes a frequently queried or independently updated fact,
  split it into a table then.

Machine epoch decision:

- The current substrate slice already has `epoch` in
  `crates/polis/src/membership/schema.sql`.
- Keep it for the current slice as an owner-issued machine row version, not as
  a global conflict clock.
- Do not add epoch columns to new tables unless the table has a clear issuer or
  an owner-enforced transition.
- Before 1.0 durable data exists, either keep this exact machine epoch
  semantics and test it, or remove the column with a greenfield schema rebuild
  and updated membership adapter tests. Do not leave both interpretations in
  the code.

### `namespaces`

Owner: product command that creates, promotes, or deletes namespaces.

```sql
CREATE TABLE namespaces (
    namespace_id TEXT NOT NULL DEFAULT '',
    owner_island_id TEXT NOT NULL DEFAULT '',
    name TEXT NOT NULL DEFAULT '',
    kind TEXT NOT NULL DEFAULT 'branch',
    source_namespace_id TEXT NOT NULL DEFAULT '',
    lifecycle TEXT NOT NULL DEFAULT 'active',
    updated_at TEXT NOT NULL DEFAULT '',
    PRIMARY KEY (namespace_id)
);

CREATE INDEX idx_namespaces_name ON namespaces (name);
CREATE INDEX idx_namespaces_lifecycle ON namespaces (lifecycle);
```

Dynamic namespace-scoped networking is deferred as a post-1.0 optimization to
reduce mesh scope for large authority islands. In 1.0, namespace is the deploy
grouping, not the network boundary. WireGuard peers are active machines in the
same authority island:

```sql
SELECT peer.*
FROM machines peer
WHERE peer.island_id = ?
  AND peer.machine_id != ?
  AND peer.lifecycle = 'active';
```

### `service_revisions`

Owner: deploy/branch/promote command coordinator.

```sql
CREATE TABLE service_revisions (
    namespace_id TEXT NOT NULL DEFAULT '',
    service_name TEXT NOT NULL DEFAULT '',
    revision_id TEXT NOT NULL DEFAULT '',
    image_ref TEXT NOT NULL DEFAULT '',
    config_digest TEXT NOT NULL DEFAULT '',
    source_json TEXT NOT NULL DEFAULT '{}',
    lifecycle TEXT NOT NULL DEFAULT 'active',
    created_at TEXT NOT NULL DEFAULT '',
    PRIMARY KEY (namespace_id, service_name, revision_id)
);

CREATE INDEX idx_service_revisions_service
  ON service_revisions (namespace_id, service_name);
```

`source_json` is acceptable when it is immutable provenance for a revision. If
source fields become query-critical, split them later.

### `service_instance_placements`

Owner: deploy/branch/promote/rollback command coordinator. This row is
committed placement and lifecycle intent. Runtime observations do not write it.

```sql
CREATE TABLE service_instance_placements (
    instance_id TEXT NOT NULL DEFAULT '',
    namespace_id TEXT NOT NULL DEFAULT '',
    service_name TEXT NOT NULL DEFAULT '',
    revision_id TEXT NOT NULL DEFAULT '',
    machine_id TEXT NOT NULL DEFAULT '',
    lifecycle TEXT NOT NULL DEFAULT 'starting',
    active_deploy_id TEXT NOT NULL DEFAULT '',
    updated_at TEXT NOT NULL DEFAULT '',
    PRIMARY KEY (instance_id)
);

CREATE INDEX idx_service_instance_placements_service
  ON service_instance_placements (namespace_id, service_name);
CREATE INDEX idx_service_instance_placements_machine
  ON service_instance_placements (machine_id);
```

### `service_instance_observations`

Owner: the machine hosting the instance. These rows are runtime observations,
not durable desired state. Route promotion may use fresh probe receipts and the
latest observation as evidence, but observations do not silently promote or
demote routes.

```sql
CREATE TABLE service_instance_observations (
    instance_id TEXT NOT NULL DEFAULT '',
    machine_id TEXT NOT NULL DEFAULT '',
    observation_id TEXT NOT NULL DEFAULT '',
    readiness TEXT NOT NULL DEFAULT 'unknown',
    health_json TEXT NOT NULL DEFAULT '{}',
    observed_at TEXT NOT NULL DEFAULT '',
    PRIMARY KEY (instance_id, machine_id, observation_id)
);

CREATE INDEX idx_service_instance_observations_instance
  ON service_instance_observations (instance_id, observed_at);
```

This split is mandatory. A coordinator lifecycle write and an owner-machine
readiness write must not race on the same Corrosion row.

### `volumes`

Owner: the current volume owner machine. A deploy or volume command
coordinator must RPC to the current owner for source writes. During a transfer,
the target machine writes receive/activate evidence and writes the final volume
row when it atomically becomes the owner.

```sql
CREATE TABLE volumes (
    namespace_id TEXT NOT NULL DEFAULT '',
    volume_id TEXT NOT NULL DEFAULT '',
    owner_machine_id TEXT NOT NULL DEFAULT '',
    backend TEXT NOT NULL DEFAULT 'zfs',
    dataset TEXT NOT NULL DEFAULT '',
    lifecycle TEXT NOT NULL DEFAULT 'active',
    source_namespace_id TEXT NOT NULL DEFAULT '',
    source_volume_id TEXT NOT NULL DEFAULT '',
    updated_at TEXT NOT NULL DEFAULT '',
    PRIMARY KEY (namespace_id, volume_id)
);

CREATE INDEX idx_volumes_owner ON volumes (owner_machine_id);
CREATE INDEX idx_volumes_source ON volumes (source_namespace_id, source_volume_id);
```

### `volume_snapshots`

Owner: the machine that owns the source volume at snapshot time.

```sql
CREATE TABLE volume_snapshots (
    snapshot_id TEXT NOT NULL DEFAULT '',
    namespace_id TEXT NOT NULL DEFAULT '',
    volume_id TEXT NOT NULL DEFAULT '',
    owner_machine_id TEXT NOT NULL DEFAULT '',
    backend_snapshot_id TEXT NOT NULL DEFAULT '',
    source_watermark TEXT NOT NULL DEFAULT '',
    created_at TEXT NOT NULL DEFAULT '',
    PRIMARY KEY (snapshot_id)
);

CREATE INDEX idx_volume_snapshots_volume
  ON volume_snapshots (namespace_id, volume_id);
```

### `deploy_commits`

Owner: deploy/branch/promote/rollback command coordinator.

```sql
CREATE TABLE deploy_commits (
    deploy_id TEXT NOT NULL DEFAULT '',
    namespace_id TEXT NOT NULL DEFAULT '',
    source_kind TEXT NOT NULL DEFAULT '',
    source_ref TEXT NOT NULL DEFAULT '',
    plan_digest TEXT NOT NULL DEFAULT '',
    previous_deploy_id TEXT NOT NULL DEFAULT '',
    state TEXT NOT NULL DEFAULT 'committed',
    committed_at TEXT NOT NULL DEFAULT '',
    PRIMARY KEY (deploy_id)
);

CREATE INDEX idx_deploy_commits_namespace
  ON deploy_commits (namespace_id, committed_at);
```

This is not a generic operations table. It is the immutable deploy header. The
state needed for history, verify, promote, and rollback lives in typed evidence
rows below, not in one opaque `result_json` blob.

### `deploy_commit_service_revisions`

Owner: deploy/branch/promote/rollback command coordinator.

```sql
CREATE TABLE deploy_commit_service_revisions (
    deploy_id TEXT NOT NULL DEFAULT '',
    namespace_id TEXT NOT NULL DEFAULT '',
    service_name TEXT NOT NULL DEFAULT '',
    revision_id TEXT NOT NULL DEFAULT '',
    source_kind TEXT NOT NULL DEFAULT '',
    source_ref TEXT NOT NULL DEFAULT '',
    PRIMARY KEY (deploy_id, namespace_id, service_name)
);
```

### `deploy_commit_instances`

Owner: deploy/branch/promote/rollback command coordinator.

```sql
CREATE TABLE deploy_commit_instances (
    deploy_id TEXT NOT NULL DEFAULT '',
    instance_id TEXT NOT NULL DEFAULT '',
    namespace_id TEXT NOT NULL DEFAULT '',
    service_name TEXT NOT NULL DEFAULT '',
    revision_id TEXT NOT NULL DEFAULT '',
    machine_id TEXT NOT NULL DEFAULT '',
    PRIMARY KEY (deploy_id, instance_id)
);
```

### `deploy_commit_routes`

Owner: deploy/branch/promote/rollback command coordinator.

```sql
CREATE TABLE deploy_commit_routes (
    deploy_id TEXT NOT NULL DEFAULT '',
    namespace_id TEXT NOT NULL DEFAULT '',
    route_id TEXT NOT NULL DEFAULT '',
    host TEXT NOT NULL DEFAULT '',
    service_name TEXT NOT NULL DEFAULT '',
    target_revision_id TEXT NOT NULL DEFAULT '',
    PRIMARY KEY (deploy_id, namespace_id, route_id)
);
```

### `deploy_commit_volume_lineage`

Owner: deploy/branch/promote/rollback command coordinator.

```sql
CREATE TABLE deploy_commit_volume_lineage (
    deploy_id TEXT NOT NULL DEFAULT '',
    namespace_id TEXT NOT NULL DEFAULT '',
    volume_id TEXT NOT NULL DEFAULT '',
    owner_machine_id TEXT NOT NULL DEFAULT '',
    source_namespace_id TEXT NOT NULL DEFAULT '',
    source_volume_id TEXT NOT NULL DEFAULT '',
    snapshot_id TEXT NOT NULL DEFAULT '',
    source_watermark TEXT NOT NULL DEFAULT '',
    irreversible_cleanup TEXT NOT NULL DEFAULT 'no',
    PRIMARY KEY (deploy_id, namespace_id, volume_id)
);
```

### `deploy_phases`

Owner: command coordinator.

```sql
CREATE TABLE deploy_phases (
    deploy_id TEXT NOT NULL DEFAULT '',
    phase_id TEXT NOT NULL DEFAULT '',
    phase_kind TEXT NOT NULL DEFAULT '',
    participant_machine_id TEXT NOT NULL DEFAULT '',
    state TEXT NOT NULL DEFAULT 'pending',
    failure_json TEXT NOT NULL DEFAULT '{}',
    started_at TEXT NOT NULL DEFAULT '',
    finished_at TEXT NOT NULL DEFAULT '',
    PRIMARY KEY (deploy_id, phase_id)
);

CREATE INDEX idx_deploy_phases_state ON deploy_phases (state);
```

### `routes`

Owner: deploy/promote/rollback coordinator.

```sql
CREATE TABLE routes (
    namespace_id TEXT NOT NULL DEFAULT '',
    route_id TEXT NOT NULL DEFAULT '',
    host TEXT NOT NULL DEFAULT '',
    service_name TEXT NOT NULL DEFAULT '',
    active_deploy_id TEXT NOT NULL DEFAULT '',
    lifecycle TEXT NOT NULL DEFAULT 'active',
    updated_at TEXT NOT NULL DEFAULT '',
    PRIMARY KEY (namespace_id, route_id)
);

CREATE INDEX idx_routes_host ON routes (host);
```

Gateway and DNS are projections from `routes` plus runtime readiness. They do
not write health back into durable truth.

## Polis Primitives

Implement small Corrosion-specific primitives:

```text
polis::store
  transaction(statements)
  query(sql, params)
  subscribe(sql, params, cursor)
  table_updates(table, cursor)

polis::identity
  load_or_create_iroh_secret(path)
  endpoint_id(secret)

polis::peers
  endpoint(config)
  rpc_server(protocol)
  rpc_client(endpoint_id, deadline)
  probe(endpoint_id, deadline)

polis::tickets
  create_bootstrap_ticket(endpoint_id, relay/discovery envelope)
  parse_bootstrap_ticket(text)

polis::membership
  machine_row query/upsert helpers
```

Authority island peer derivation is a Ployz adapter query over store
primitives, not a Polis product API. Dynamic namespace-scoped peer derivation
is deferred as a post-1.0 optimization.

Avoid these APIs:

```text
polis::machines.join(...)
polis::namespaces.create_branch(...)
polis::deploy.record_ready(...)
polis::capacity.reserve(...)
```

Those are Ployz product concepts. They belong in Ployz modules and adapters.

## Ployz Adapter Shape

Adapters are allowed to be purposeful and a little thick:

```text
crates/ployz/src/adapters/polis/
  machine_membership.rs
  namespace.rs
  deployment_store.rs
  runtime_rpc.rs
  volume_store.rs
  route_store.rs
```

Each adapter should:

- translate product newtypes to SQL/RPC primitives;
- own all Corrosion statement strings for that product port;
- document row ownership rules beside writes;
- use idempotent upserts;
- expose product-shaped ports to ordinary Ployz modules.

## Daemon Runtime Responsibilities

The daemon owns runtime components and adoption:

- iroh endpoint and RPC server lifecycle;
- Corrosion process lifecycle and schema apply;
- WireGuard configuration derived from active machines in the authority island;
- runtime backend command execution;
- gateway/DNS projection;
- local state files for private keys and backend caches.

Startup adoption order:

1. Start substrate.
2. Read local machine row and active authority island peers.
3. Rebuild WireGuard full-mesh peer config.
4. Inspect runtime containers/services/volumes.
5. Rebuild gateway/DNS from durable rows.
6. Expose health/status.

Do not restart working services just because the daemon restarted.

## Implementation Units

### U1. Corrosion Schema Loader And Store Primitive

Files:

- `crates/polis/src/store.rs`
- `crates/polis/src/schema/`
- `crates/polis/tests/`

Work:

- Apply SQL schema files through Corrosion-supported paths.
- Add transaction/query helpers over `corro-client`.
- Add subscription cursor model and EOQ handling.
- Add table update stream helper only for invalidation use cases.

Acceptance:

- A local Corrosion test creates schema, writes rows, queries rows, and resumes
  a subscription after reconnect.

### U2. Iroh Identity, Ticket, And Peer RPC

Files:

- `crates/polis/src/peers/`
- `crates/polis/src/identity.rs`

Work:

- Load/create iroh secret key.
- Start endpoint with configured relay/discovery.
- Start RPC server.
- Implement bounded peer probe.
- Implement ticket create/parse for bootstrap only.

Acceptance:

- Two local daemons can exchange endpoint IDs by ticket, probe each other, and
  run one typed RPC without Corrosion involvement.

### U3. Machine Membership Vertical Slice

Files:

- `crates/polis/src/membership/`
- `crates/ployz/src/machine.rs`
- `crates/ployz/src/adapters/polis/machine_membership.rs`

Work:

- Implement `machines` schema.
- Implement owner-written machine upsert.
- Implement observe/query stream.
- Implement join flow using iroh RPC plus machine row write.

Acceptance:

- One node can join another, both observe the machine row, and restart keeps
  the same endpoint identity.

### U4. Authority Island Mesh And Namespace Rows

Files:

- `crates/ployz/src/adapters/polis/namespace.rs`
- daemon WireGuard backend module

Work:

- Add `namespaces`.
- Add active authority island peer query.
- Rebuild WireGuard config from active authority island machines.
- Add diagnostics for silent/no-peer states.

Acceptance:

- Two active machines in the same authority island derive each other as peers.
- Namespace changes do not rewrite WireGuard policy in 1.0.

### U5. Runtime RPC And Backend Adoption

Files:

- `crates/ployz/src/runtime/`
- daemon runtime backend crate/module

Work:

- Define internal RPC commands for start, stop, verify, drain, cleanup,
  volume snapshot/clone/move, logs.
- Keep these separate from public CLI/API request types.
- Add timeouts and structured failures.
- Adopt existing runtime resources on daemon restart.

Acceptance:

- Deploy executor can start and verify a trivial workload on a chosen machine.
- Restart does not break last-good workload.

### U6. Deploy Evidence Store

Files:

- `crates/ployz/src/deploy/`
- `crates/ployz/src/adapters/polis/deployment_store.rs`

Work:

- Add `service_revisions`, `service_instance_placements`,
  `service_instance_observations`, `deploy_commits`, typed deploy commit
  evidence rows, `deploy_phases`, and `routes`.
- Write immutable deploy commits.
- Write phase rows as command evidence.
- Add history/verify reads.

Acceptance:

- A single-service deploy writes enough durable evidence for
  `ployz deploy history` and `ployz rollback preview`.

## Test Matrix

- Corrosion unit tests for schema and row upserts.
- Local two-node iroh RPC test without Corrosion.
- Local two-node Corrosion sync test without runtime backend.
- Authority island peer derivation test.
- Daemon restart adoption test.
- Single-service deploy evidence test.
- Substrate failure tests: peer unavailable, stale row, subscription resume
  failure, partial initial subscription before EOQ.

## Open Questions

- Whether `deploy_phases.failure_json` should be split into typed columns after
  the first failure surfaces are known.
- Whether hosted cloud needs a context object in open core, or only local CLI
  contexts plus public API are enough for 1.0.
