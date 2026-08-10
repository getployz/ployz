# The Corrosion Row Model

This is the writable v2 row contract. The companion DDL is
[corrosion-schema-v1.sql](corrosion-schema-v1.sql), and the identity decision is
recorded in [ADR 0042](../adr/0042-canonical-names-are-resource-identities.md).
There is no older v2 shape to accept or migrate.

## Admission rule

A fact belongs in Corrosion only when another machine must watch it live.
Duroxide history remains in node-local SQLite and covers only bounded host
prepare and retire work. Private keys, secret environment values, deploy-time
secret payloads, and TLS keys never enter Corrosion.

## Identity rule

Ployz uses canonical names as identities. It does not mint a second random ID
for a resource that already has a durable name.

| Table | Key | Meaning |
|---|---|---|
| `cluster` | cluster name | one cluster identity |
| `machines` | machine name | one admitted host |
| `peers` | peer name | one operator principal; separate from machines |
| `tokens` | token name | one show-once join credential |
| `namespaces` | namespace name | complete name-keyed service intent and lifecycle boundary |
| `route_bindings` | hostname | one ingress binding |
| `controller` | cluster name | advisory preferred-controller appointment |
| `machine_endpoints` | machine name | one machine's complete routable endpoint testimony |
| `machine_status` | machine name | machine-owned testimony |
| `operations` | `<namespace>/<deploy>` | one namespace-wide deploy result |
| `cert_holdings` | `<machine>:<hostname>` | one gateway's certificate testimony |
| `acme_http01` | ACME challenge token | one public challenge |

Docker container IDs remain private runtime handles on their owning machine.
Controller revisions are monotonic integers. Randomness remains only where it
is the substance of the value: secrets, cryptographic keys, Corrosion internals,
and external runtime handles.

Machines and peers remain separate tables and principals. They have different
admission, authorization, transport, and lifecycle laws even though both are
identified by canonical names.

## Namespace snapshots and services

A deploy request names a namespace and deploy and supplies the complete desired
name-keyed service object for that namespace. The preferred controller observes reality,
plans every requested service together, prepares bounded host effects, then
commits the namespace snapshot. Services omitted from the request are removed,
and obsolete containers are retired.

The namespace is therefore the atomic reconciliation boundary. A service is
still the workload and addressability boundary: image, placement, replicas,
containers, routes, internal DNS, and logs all retain the service name. This
supports multiple services cleanly without giving each service an unrelated
random identity.

Deploy names are one-shot within their namespace. A used `<namespace>/<deploy>`
key is refused before host effects; recovery after an accepted or abandoned
attempt uses a fresh deploy name.

## Write and conflict law

Every ordinary authority row is addressed by its semantic key. The preferred
controller serializes cluster mutations in memory and followers forward writes
to it. A partition can still produce competing writes to the same key; Corrosion
resolves those whole-document writes. Ployz does not retain duplicate-name rows,
shadow indexes, ambiguity selectors, `--id` escape hatches, or merge field-level
intent.

For scary writes, the controller reads current reality, computes one plan, and
uses exact observed-state conditions where data safety requires them. After
controller loss, a later attempt starts again from reality. It does not recover
an abandoned controller's in-memory history.

Machine and peer roster rows are accepted only after their transport matches the
cluster provider. A malformed or foreign row is skipped and surfaced to
diagnostics; it is never repaired by inventing another identity.

## Ownership and cleanup

Operator commands own cluster, machine, peer, token, namespace, and route
intent. A service is nested intent inside its namespace document. Machines own
their bounded testimony. The controller writes namespace deploy results after
host preparation.

Removal is explicit. Removing a machine also sweeps its machine-owned endpoint
testimony. A namespace deploy replaces the complete desired service set for that
namespace. There is no background intent reaper or public workflow-history log.

## Wire conventions

- Every document carries `cluster_id`; readers reject foreign cluster rows.
- Operator-authority documents carry authenticated `written_by` provenance and
  a canonical `written_at` timestamp.
- All timestamps serialize as UTC RFC 3339 with nine fractional digits.
- Every document carries integer `v`. Readers tolerate additive fields and skip
  newer versions they cannot interpret.
- New row shapes roll out only after compatible binaries are present on every
  machine.
- Empty, malformed, foreign, noncanonical, or provider-mismatched rows remain
  visible as diagnostic evidence.

## Secret and certificate rule

Join tokens are named. The issued blob contains that name plus a random secret;
the token row stores only `sha256(secret)`, creation/expiry times, and public
provenance. Revocation deletes the named row. Environment values remain outside
Corrosion; the namespace intent document carries service fingerprints only.

TLS and ACME account private keys stay machine-local. `cert_holdings` is
per-machine testimony, while `acme_http01` contains only public challenge
material. Certificate selection and renewal derive from current route bindings;
no shared secret or certificate-key row exists.
