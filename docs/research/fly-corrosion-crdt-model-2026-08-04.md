# How Fly uses Corrosion and CRDTs

Snapshot: **2026-08-04**. Question: does Fly model orchestration as a deep
CRDT or immutable fact system, or does it use Corrosion more narrowly? Sources
are Fly engineering posts and Infra Log entries, plus the maintained Corrosion
and cr-sqlite repositories. Exact production DDL and private application code
are not public, so the boundary between evidence and inference matters here.

## Conclusion

Fly leans into CRDTs for **replication and convergence**, but avoids most
semantic conflicts through **single-owner state**. A physical worker is the
source of truth for the Machines it runs. It publishes current Machine state
into Corrosion, where cr-sqlite resolves concurrent writes per column. Readers
then build disposable local projections from the converged database
([Fly's Corrosion architecture](https://fly.io/blog/corrosion/),
[worker-owned orchestration](https://fly.io/blog/carving-the-scheduler-out-of-our-orchestrator/)).

```text
central API --places work--> physical worker
                              source of truth
                                    |
                                    | complete current Machine state
                                    v
                              Corrosion SQLite
                              per-column CRDTs
                                    |
                         +----------+----------+
                         v                     v
                   proxy Catalog          other readers
                   derived view           derived views
```

This is not an immutable-fact log in Corrosion, and the public sources do not
show a Fly application layer that semantically folds competing orchestration
intents. The accurate shorthand is:

```text
ownership decides who should write
CRDT rules decide what converges if writes race
rebuildable views decide how consumers recover
```

## Ownership does most of the conflict prevention

- Fly says each worker is authoritative for its own workloads and that
  different workers' updates therefore "almost never conflict." Its central
  API places work, but it does not maintain a consensus-backed global
  orchestration picture. Corrosion distributes the global working-set view
  needed for placement and routing
  ([Corrosion](https://fly.io/blog/corrosion/),
  [scheduler architecture](https://fly.io/blog/carving-the-scheduler-out-of-our-orchestrator/)).
- This ownership invariant is operational, not something cr-sqlite infers.
  Fly reported that moving a Machine between workers broke the assumption and
  produced races and design changes
  ([Making Machines Move](https://fly.io/blog/machine-migrations/)).
- Not all production domains are worker-owned. For example, Fly's central
  GraphQL API publishes organization information to the global Corrosion
  cluster for the Machines API to read. That is still explicit producer
  ownership, not arbitrary multi-writer editing
  ([May 30, 2026 incident](https://fly.io/infra-log/)).
- A Fly engineer describes the wider split directly: Machines, volumes, and
  services originate in each host's BoltDB, while apps and IP assignments
  originate in Fly's central RDS database. Corrosion holds copies of both; it
  is not authoritative for either
  ([production sync pipeline](https://community.fly.io/t/improved-data-sync-pipeline-for-corrosion/25988)).

**Inference:** Corrosion's merge rule is a convergence backstop, not Fly's
business-level conflict-resolution policy. Correctness primarily comes from
assigning one authoritative producer per resource or domain.

## The stored model is mutable current state

- Corrosion marks ordinary SQLite tables as CRDT-managed. cr-sqlite records
  changes for each column and Corrosion batches and gossips them
  ([Fly overview](https://fly.io/blog/corrosion/),
  [Corrosion CRDT documentation](https://github.com/superfly/corrosion/blob/98d76982d43da4a2f339dbc47a3fc0cb30bd5a8f/doc/crdts.md#L9-L29)).
- Existing-row conflicts are resolved by greatest `col_version`, then greatest
  SQLite value, then `site_id`; the last tiebreaker is effectively random
  because each Corrosion actor has a random site ID
  ([documented conflict example](https://github.com/superfly/corrosion/blob/98d76982d43da4a2f339dbc47a3fc0cb30bd5a8f/doc/crdts.md#L171-L239)).
  The result is one converged value, not a retained set of conflicting intents.
- The maintained cr-sqlite documentation calls its current approach
  **history-free**: it keeps current state, automatically merges rows as maps
  of column CRDTs, and offers no manual conflict merge. Its causal event-log
  model is documented separately as an unimplemented v2 direction
  ([cr-sqlite model](https://github.com/vlcn-io/cr-sqlite/blob/ed99ee5e1aebe0cf9ecbb56ea21fd8494e053dba/README.md#L165-L231)).
- Fly formerly emitted partial Machine updates. It now republishes the entire
  current data set for a Machine after a change; Corrosion discards no-op cell
  updates before gossip. Fly credits this with eliminating classes of bugs
  ([Corrosion, "Iteration"](https://fly.io/blog/corrosion/)).

Fly's worker does retain an append-only log of local operations in BoltDB, but
Corrosion is described as the fleet-wide SQLite **view** of resources, not that
operation log
([worker storage model](https://fly.io/blog/carving-the-scheduler-out-of-our-orchestrator/)).

## Rows, schema, IDs, and derived views

- Public sources establish relational domains for Machines, services, health
  information, organizations, and app-to-region routing. The November 2024
  incident report identifies a very large table containing configured services
  for Fly Machines. They do **not** publish the complete production DDL, JSON
  shapes, provenance columns, primary-key allocation algorithm, or a domain
  merge layer
  ([production schema incident](https://fly.io/infra-log/),
  [Corrosion overview](https://fly.io/blog/corrosion/)).
- Stock Corrosion accepts ordinary `CREATE TABLE` and `CREATE INDEX` schema
  files. Primary keys must be non-null; secondary unique indexes are forbidden;
  and non-null columns need defaults. Its example uses an integer primary key,
  so Corrosion itself does not require ULIDs or content-derived identifiers
  ([schema rules](https://github.com/superfly/corrosion/blob/98d76982d43da4a2f339dbc47a3fc0cb30bd5a8f/doc/schema.md#L1-L30)).
- Corrosion does use a random 16-byte site/actor ID internally for writer
  bookkeeping and conflict tiebreaking. That is separate from application row
  identity
  ([actor IDs](https://github.com/superfly/corrosion/blob/98d76982d43da4a2f339dbc47a3fc0cb30bd5a8f/doc/crdts.md#L31-L42)).
- `fly-proxy` treats Corrosion as its routing information base and builds an
  in-memory `Catalog` as a fast forwarding view. Fly is also moving from one
  global detail database to regional fine-grained databases plus a global
  app-to-region map, reducing the broadcast and failure domain rather than
  adding a stronger CRDT
  ([Catalog/RIB-to-FIB model](https://fly.io/blog/parking-lot-ffffffffffffffff/),
  [regionalization](https://fly.io/blog/corrosion/)).
- Some internal services receive a complete read-only SQLite replica through
  LiteFS and run their own SQL and views, rather than join the gossip cluster
  ([Skip the API, Ship Your Database](https://fly.io/blog/skip-the-api/)).

## Failure behavior: repair projections, do not ask the CRDT to understand them

Fly's documented failures are mostly semantic, schema, or consumer failures
that the CRDT correctly amplified:

- A valid but unusual virtual-service configuration triggered a proxy deadlock.
  Corrosion spread the configuration fleet-wide within seconds; restarting a
  proxy only made it consume the poisonous update again. Fly disabled creation
  of that configuration, fixed the reader, added watchdogs, and pursued
  regional blast-radius limits
  ([September 2024 incident](https://fly.io/infra-log/2024-09-07/),
  [engineering analysis](https://fly.io/blog/corrosion/)).
- Adding a nullable column to a large CRDT table caused a fleet-wide backfill
  storm. Repeated otherwise-useless writes have also saturated Fly's network.
  Their recovery included rate limits, checkpoint restore, and rebuilding the
  database from external sources of truth
  ([Corrosion failure history](https://fly.io/blog/corrosion/)).
- Stock Corrosion explicitly documents whole-cluster **reseeding** when bad data
  is spreading faster than it can be repaired. Reseeding may discard writes
  after the chosen snapshot
  ([reseed runbook](https://github.com/superfly/corrosion/blob/98d76982d43da4a2f339dbc47a3fc0cb30bd5a8f/doc/reseeding.md#L1-L24)).
- Fly's production reseeder restores a known-good snapshot and re-inserts rows
  from the authoritative BoltDB/RDS stores. After one bad insert repeatedly
  stalled a table's reseed, Fly split reseeding into rate-limited parallel ID
  ranges so one failed range does not stop the others, and began exercising the
  path weekly. This isolates repair work, but it is not a documented durable
  poisoned-row quarantine
  ([production reseed pipeline](https://community.fly.io/t/improved-data-sync-pipeline-for-corrosion/25988)).
- Schema coordination remains an application/deployment obligation. In May
  2026, some hosts had the new schema but had not reloaded it; a proxy then
  became healthy before discovering its queries could not run. Fly changed
  Corrosion to surface reload failures and changed proxy startup to prepare
  every query before becoming healthy
  ([May 19, 2026 incident](https://fly.io/infra-log/proxy-corrosion-sin/)).
- In June 2026, a blocked update channel contributed to Corrosion OOMs. Fly
  chose to drop update notifications under pressure because the proxy can
  tolerate missed notifications. That makes notifications hints over a
  reloadable converged database, not an authoritative event log
  ([June 23, 2026 incident](https://fly.io/infra-log/corrosion-ooms-systemd/)).

No reviewed public source describes a durable per-row quarantine, typed
semantic rejection, or conflict inbox. Fly has described "poisonous" updates,
but the published remedies are reader fixes, producer shutdown, watchdogs,
regionalization, backfill, and cluster reseeding. Absence from public material
does not prove no private quarantine mechanism exists.

## What the Fly comparison establishes

```text
Fly-like shallow CRDT

stable opaque resource identity
        +
one authoritative writer per ownership domain
        +
complete mutable snapshots
        +
per-column CRDT convergence
        +
rebuildable consumer projections
```

This model is strong when conflicts should be rare and indicate broken
ownership. It does not preserve competing intentions or explain which intent
was better. A system that expects independent agents to edit the same resource
concurrently would need an additional policy: stricter ownership, explicit
claims, multi-value conflict evidence, or an immutable fact/fold model. Fly's
public Corrosion design does not answer that semantic question for it.
