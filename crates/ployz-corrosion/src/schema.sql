-- IMPORTANT: replicated rows in corrosion/cr-sqlite are not published atomically
-- at the logical-record level. Independent columns can arrive at different
-- times, so "the row exists" does NOT mean "the record is valid".
--
-- Store contract:
-- - A replicated row is unpublished while payload_json = ''.
-- - Readers must only surface rows where payload_json <> ''.
-- - Once a row is published, payload_json must contain the full logical record.
-- - We do NOT use partially-filled rows, readiness flags, or consumer-side
--   heuristics to guess whether a record is usable.
--
-- Column rule:
-- - Keep a column independent only if it is true identity/filter metadata:
--   something we need for PRIMARY KEY, deletion, or cheap scoped queries.
-- - Everything else goes in payload_json, even if it feels "nice" to normalize.
--   If a field is semantically part of one logical record, splitting it into its
--   own replicated column re-introduces partial publication bugs.
--
-- When changing this schema:
-- - If a field can be observed independently without making the record invalid,
--   it may stay outside payload_json.
-- - If consumers reason about multiple fields together, keep them inside one
--   payload_json blob and publish them together.
-- - Prefer adding filter columns only when a real query needs them.
-- - Do not add default-filled business columns outside payload_json.
-- - Do not add "is_ready" style columns as a substitute for atomic publication.
--   The publication boundary is payload_json itself.

-- Central registry of all machines in the mesh. Each machine has a WireGuard
-- keypair, an overlay IP, a subnet allocation, and advertised endpoints for
-- peer connectivity. Participation (enabled/disabled/draining) controls whether
-- a machine receives workloads. Machines join as disabled and are promoted after
-- enough healthy heartbeats accumulate.
--
-- Why these columns are shaped this way:
-- - machine_id stays independent because it is the row identity, delete target,
--   and stable lookup key.
-- - Every other machine field must publish together: public_key, overlay_ip,
--   subnet, bridge_ip, endpoints, status, participation, heartbeats, labels,
--   and timestamps are not independently meaningful to consumers.
CREATE TABLE IF NOT EXISTS machines (
    machine_id TEXT NOT NULL PRIMARY KEY,
    payload_json TEXT NOT NULL DEFAULT '' CHECK (payload_json = '' OR json_valid(payload_json))
);

-- One-time-use tokens for joining the mesh. Each invite has a TTL; consuming an
-- invite atomically deletes the row. Expired or already-consumed invites are
-- rejected.
--
-- Why these columns are shaped this way:
-- - invite_id stays independent because it is the lookup/delete key.
-- - expires_at and any future invite metadata live in payload_json because an
--   invite is only meaningful as one complete record.
CREATE TABLE IF NOT EXISTS invites (
    invite_id TEXT NOT NULL PRIMARY KEY,
    payload_json TEXT NOT NULL DEFAULT '' CHECK (payload_json = '' OR json_valid(payload_json))
);

-- Append-only ledger of service spec versions. Each revision is content-
-- addressed by (namespace, service, revision_hash) and inserted via INSERT OR
-- IGNORE, so duplicate publishes are idempotent.
--
-- Why these columns are shaped this way:
-- - namespace, service, and revision_hash stay independent because together they
--   are the revision identity and the natural query key.
-- - The revision body itself stays in payload_json. That includes spec_json and
--   creation metadata. Even though revisions are immutable, we still want
--   "record exists" to imply "full revision is readable".
CREATE TABLE IF NOT EXISTS service_revisions (
    namespace TEXT NOT NULL DEFAULT '',
    service TEXT NOT NULL DEFAULT '',
    revision_hash TEXT NOT NULL DEFAULT '',
    payload_json TEXT NOT NULL DEFAULT '' CHECK (payload_json = '' OR json_valid(payload_json)),
    PRIMARY KEY (namespace, service, revision_hash)
);

-- Mutable published routing intent for one logical service. This is the atomic
-- desired-state record consumed by routing. It may reference multiple immutable
-- revisions for rollout strategies such as canary or blue/green.
--
-- Why these columns are shaped this way:
-- - namespace and service stay independent because they identify the logical
--   service release row and let us query releases by namespace cheaply.
-- - The mutable routing intent must publish as one blob: primary revision,
--   referenced revisions, rollout policy, traffic split, slot assignments, and
--   deploy metadata are one coherent state machine.
-- - This table intentionally replaces the old head/slot split. Those normalized
--   columns looked convenient, but they leaked impossible intermediate states
--   under replication. The atomic unit for desired routing is one service
--   release, not "current head" and "current slots" independently.
CREATE TABLE IF NOT EXISTS service_releases (
    namespace TEXT NOT NULL DEFAULT '',
    service TEXT NOT NULL DEFAULT '',
    payload_json TEXT NOT NULL DEFAULT '' CHECK (payload_json = '' OR json_valid(payload_json)),
    PRIMARY KEY (namespace, service)
);

-- Runtime state of individual container instances. Lifecycle goes from pending
-- → running (ready=1) → draining → deleted. Traffic is only routed to ready,
-- non-draining instances.
--
-- Why these columns are shaped this way:
-- - instance_id stays independent because it is the row identity.
-- - namespace, service, and machine_id also stay independent because they are
--   true filter metadata used to query live state by scope/owner.
-- - The rest of instance state stays in payload_json: slot_id, revision_hash,
--   deploy_id, container id, ports, readiness, drain state, errors, and
--   timestamps must publish together to avoid fake pending/ready/draining rows.
CREATE TABLE IF NOT EXISTS instance_status (
    instance_id TEXT NOT NULL PRIMARY KEY,
    namespace TEXT NOT NULL DEFAULT '',
    service TEXT NOT NULL DEFAULT '',
    machine_id TEXT NOT NULL DEFAULT '',
    payload_json TEXT NOT NULL DEFAULT '' CHECK (payload_json = '' OR json_valid(payload_json))
);

-- Tracks the lifecycle of each deployment operation. State progresses from
-- applying → committed (or failed/cleanup_pending).
--
-- Why these columns are shaped this way:
-- - deploy_id stays independent because it is the row identity and lookup key.
-- - namespace stays independent because deploy history is commonly queried by
--   namespace.
-- - The rest of deploy lifecycle state stays in payload_json: coordinator,
--   manifest hash, state, timestamps, and summary must publish together so a
--   visible deploy row is always a coherent deploy record.
CREATE TABLE IF NOT EXISTS deploys (
    deploy_id TEXT NOT NULL PRIMARY KEY,
    namespace TEXT NOT NULL DEFAULT '',
    payload_json TEXT NOT NULL DEFAULT '' CHECK (payload_json = '' OR json_valid(payload_json))
);
