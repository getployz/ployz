-- Corrosion schema, first draft (wayfinder: the row model, issue #785).
--
-- Every table is a JSON document plus virtual generated columns for indexed
-- reads. SQL DDL is additive-only forever (Corrosion refuses destructive
-- changes); document shape evolves inside the JSON, governed by the `v`
-- field. Every document carries `v` (integer schema version) and
-- `cluster_id` (the cluster ULID minted by `ployz init`).
--
-- Every operator-authority document also carries top-level `written_by`
-- (the nested OperationInitiator for the authenticated Principal) and
-- `written_at` (the canonical Corrosion timestamp). Every document timestamp
-- serializes as UTC with exactly nine
-- fractional digits; readers parse and compare instants rather than raw text.
--
-- Operator-minted row keys and every cross-row reference are domain-typed
-- canonical ULIDs: exactly 26 uppercase Crockford Base32 characters. Human
-- names are lookup handles, never references. The v1 types are ClusterId,
-- MachineRowId, PeerId, TokenId, NamespaceRowId, ServiceRowId, OperationRowId,
-- and RouteBindingRowId.
--
-- No v2 product writer ships an earlier document shape. Required v1 fields
-- are part of this contract without a compatibility or migration layer.
--
-- Ownership, PK, sweep, and secrecy rules live in corrosion-row-model.md.

-- ─── Operator authority ─────────────────────────────────────────────────────

-- One row: cluster identity and cluster-wide settings.
-- PK = ClusterId, a canonical ULID minted once by `ployz init`.
CREATE TABLE cluster (
    id TEXT NOT NULL PRIMARY KEY,
    document TEXT NOT NULL DEFAULT '{}'
);

-- The roster. One row per admitted machine; Keeper converges WG peers,
-- eBPF, sysctls, and firewall from these rows (never from an empty set).
-- PK = MachineRowId, a canonical ULID minted at admission. Transport must match
-- the ClusterDocument provider before this row may enter a roster or name
-- claim fold. The public key is write-once and subnet_v4 is required.
CREATE TABLE machines (
    id TEXT NOT NULL PRIMARY KEY,
    document TEXT NOT NULL DEFAULT '{}',
    name TEXT GENERATED ALWAYS AS (json_extract(document, '$.name')) VIRTUAL,
    lifecycle TEXT GENERATED ALWAYS AS (json_extract(document, '$.lifecycle')) VIRTUAL
);
CREATE INDEX machines_name ON machines (name);

-- Non-machine mesh peers: operator laptops and Cloud. Operator authority;
-- swept by peer rm. PK = PeerId. Peer transport has no subnet_v4 field; a
-- document that carries one is malformed. Transport must match the
-- ClusterDocument provider before this row may enter a roster or name claim
-- fold.
CREATE TABLE peers (
    id TEXT NOT NULL PRIMARY KEY,
    document TEXT NOT NULL DEFAULT '{}',
    name TEXT GENERATED ALWAYS AS (json_extract(document, '$.name')) VIRTUAL
);
CREATE INDEX peers_name ON peers (name);

-- Join and API tokens. The secret never enters the row: the issued string is
-- pz_<token-ulid>.<32-byte-random-base64>; the row keeps sha256(secret part).
-- Expiry and revocation are checked at point of use, never swept on a timer.
-- `token revoke` deletes the row (revoke = delete = cleanup; no `token rm`).
-- PK = TokenId.
CREATE TABLE tokens (
    id TEXT NOT NULL PRIMARY KEY,
    document TEXT NOT NULL DEFAULT '{}',
    kind TEXT GENERATED ALWAYS AS (json_extract(document, '$.kind')) VIRTUAL
);
CREATE INDEX tokens_kind ON tokens (kind);

-- Namespace registry. PK = NamespaceRowId, a canonical ULID; name uniqueness
-- is a wait-for-quiet claim with the lowest-ULID reader rule as backstop.
CREATE TABLE namespaces (
    id TEXT NOT NULL PRIMARY KEY,
    document TEXT NOT NULL DEFAULT '{}',
    name TEXT GENERATED ALWAYS AS (json_extract(document, '$.name')) VIRTUAL
);
CREATE INDEX namespaces_name ON namespaces (name);

-- Committed serving state: one row per service per namespace, written only
-- at deploy promotion. Carries env_fingerprints {KEY: sha256 hex of the
-- JSON-encoded value} so Cloud diffs env without values ever replicating.
-- PK is ServiceRowId and namespace_id is NamespaceRowId.
CREATE TABLE services (
    id TEXT NOT NULL PRIMARY KEY,
    document TEXT NOT NULL DEFAULT '{}',
    namespace_id TEXT GENERATED ALWAYS AS (json_extract(document, '$.namespace_id')) VIRTUAL,
    name TEXT GENERATED ALWAYS AS (json_extract(document, '$.name')) VIRTUAL
);
CREATE INDEX services_namespace_id ON services (namespace_id);
CREATE INDEX services_name ON services (name);

-- Hostname → service + endpoint port. PK is RouteBindingRowId; service_id is
-- ServiceRowId and namespace_id is NamespaceRowId.
-- Route Binding Identity means detach + recreate is a new identity even for
-- the same hostname. Hostname uniqueness = wait-for-quiet claim + lowest-ULID
-- rule.
CREATE TABLE route_bindings (
    id TEXT NOT NULL PRIMARY KEY,
    document TEXT NOT NULL DEFAULT '{}',
    hostname TEXT GENERATED ALWAYS AS (json_extract(document, '$.hostname')) VIRTUAL,
    service_id TEXT GENERATED ALWAYS AS (json_extract(document, '$.service_id')) VIRTUAL,
    namespace_id TEXT GENERATED ALWAYS AS (json_extract(document, '$.namespace_id')) VIRTUAL
);
CREATE INDEX route_bindings_hostname ON route_bindings (hostname);
CREATE INDEX route_bindings_service_id ON route_bindings (service_id);
CREATE INDEX route_bindings_namespace_id ON route_bindings (namespace_id);

-- ─── Machine authority (exactly one writing machine per row) ────────────────

-- Redacted runtime testimony: one row per running Ployz-managed container,
-- written only by the machine running it. Never carries env or command
-- lines. PK = Docker's own container id (random, never reused).
-- Documents carry `ip` (bridge IPv4) and `deploy` (revision): DNS serves
-- A records from ip gated on the service's active_deploy, and Keeper
-- builds the namespace-isolation map from ip — the container-plane
-- ticket (#801) and Keeper's second named converge family. All cross-row ids
-- are their distinct typed canonical ULIDs.
CREATE TABLE containers (
    id TEXT NOT NULL PRIMARY KEY,
    document TEXT NOT NULL DEFAULT '{}',
    machine_id TEXT GENERATED ALWAYS AS (json_extract(document, '$.machine_id')) VIRTUAL,
    service_id TEXT GENERATED ALWAYS AS (json_extract(document, '$.service_id')) VIRTUAL,
    namespace_id TEXT GENERATED ALWAYS AS (json_extract(document, '$.namespace_id')) VIRTUAL,
    deploy TEXT GENERATED ALWAYS AS (json_extract(document, '$.deploy')) VIRTUAL
);
CREATE INDEX containers_machine_id ON containers (machine_id);
CREATE INDEX containers_service_id ON containers (service_id);
CREATE INDEX containers_namespace_id ON containers (namespace_id);
CREATE INDEX containers_deploy ON containers (deploy);

-- Slow-changing per-machine testimony: versions, arch, capacity. One row
-- per machine, PK = MachineRowId. Never liveness — liveness is WG
-- last-handshake age at the point of use, never stored truth.
CREATE TABLE machine_status (
    machine_id TEXT NOT NULL PRIMARY KEY,
    document TEXT NOT NULL DEFAULT '{}'
);

-- Per-route gateway projection and process-health testimony. One row per
-- gateway process owner, PK = MachineRowId. This diagnostic feedback never
-- decides deploy success or cluster truth.
CREATE TABLE gateway_observations (
    machine_id TEXT NOT NULL PRIMARY KEY,
    document TEXT NOT NULL DEFAULT '{}'
);

-- Operation summaries (evidence ticket #783): one row per OperationRowId, at
-- most three
-- summary-state writes — created, optional running, terminal. Written only
-- by the executing machine; the terminal write is final. Heartbeat refreshes
-- rewrite only the document's top-level heartbeat_at between those writes
-- and never touch a terminal row. Detail is driver-local JSONL, never rows.
CREATE TABLE operations (
    id TEXT NOT NULL PRIMARY KEY,
    document TEXT NOT NULL DEFAULT '{}',
    kind TEXT GENERATED ALWAYS AS (json_extract(document, '$.kind')) VIRTUAL,
    state TEXT GENERATED ALWAYS AS (json_extract(document, '$.state')) VIRTUAL,
    machine_id TEXT GENERATED ALWAYS AS (json_extract(document, '$.machine_id')) VIRTUAL
);
CREATE INDEX operations_kind ON operations (kind);
CREATE INDEX operations_state ON operations (state);
CREATE INDEX operations_machine_id ON operations (machine_id);

-- Cert possession testimony (unified-cert ticket #792): one row per
-- (gateway, hostname), written only by that gateway when it issues or
-- fetches. PK embeds the MachineRowId so no two machines can address the
-- same row. Readers derive everything: current cert = the fingerprint with
-- the chronologically greatest typed expiry; fetch sources = machines whose
-- row carries it.
-- Candidate rows are parsed first and expiry maxima are chronological typed
-- comparisons; the generated TEXT value is not an authority for ordering.
-- Key material is machine-local on holders — never rows.
CREATE TABLE cert_holdings (
    id TEXT NOT NULL PRIMARY KEY,   -- "<MachineRowId>:<hostname>"
    document TEXT NOT NULL DEFAULT '{}',
    hostname TEXT GENERATED ALWAYS AS (json_extract(document, '$.hostname')) VIRTUAL,
    machine_id TEXT GENERATED ALWAYS AS (json_extract(document, '$.machine_id')) VIRTUAL,
    expires_at TEXT GENERATED ALWAYS AS (json_extract(document, '$.expires_at')) VIRTUAL
);
CREATE INDEX cert_holdings_hostname ON cert_holdings (hostname);
CREATE INDEX cert_holdings_machine_id ON cert_holdings (machine_id);

-- HTTP-01 challenge routing: key authorizations are public by design, so
-- the value rides the row openly. Written by the issuing gateway, served
-- by every gateway, swept by the issuer when the order settles.
CREATE TABLE acme_http01 (
    id TEXT NOT NULL PRIMARY KEY,   -- the ACME challenge token (unique per order)
    document TEXT NOT NULL DEFAULT '{}',
    machine_id TEXT GENERATED ALWAYS AS (json_extract(document, '$.machine_id')) VIRTUAL
);
CREATE INDEX acme_http01_machine_id ON acme_http01 (machine_id);
