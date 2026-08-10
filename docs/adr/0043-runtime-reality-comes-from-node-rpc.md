# Runtime Reality Comes From Node RPC

Corrosion is the replicated publication surface, not a database of live
services or containers. A controller that must decide what a machine can
start, stop, retain, or replace asks that machine over bounded HTTP. The
machine answers from Docker at the point of use.

One deploy request is the complete desired service set for one namespace.
Omitting a service removes it, and omitted service fields use fixed defaults;
they do not inherit values from an earlier Corrosion row. The controller plans
from that request, accepted roster policy, and fresh node replies. Operation
rows remain evidence only.

## Corrosion publications

The controller publishes the complete serving intent for a namespace by
replacing one row keyed by the canonical namespace name. That document contains
a name-keyed service map. Replacing one namespace row makes a multi-service
cutover one decision without a transaction that deletes and recreates many
Service rows.

Each machine publishes one complete endpoint-testimony row keyed by its
canonical machine name. An endpoint is identified by namespace, service,
deploy, and replica slot and contains the observed endpoint IP. It never
contains Docker's random container id. Gateway and DNS join the namespace's
serving intent with these machine-owned endpoint reports.

```text
complete deploy request + roster policy + node RPC/Docker
                         |
                         v
               one namespace intent row

Docker -> one endpoint report per machine -> Gateway / DNS
```

Endpoint reports are a read projection with explicit observation time. They do
not authorize cleanup, prove that a container still exists, or replace a node
inspection. Prepared endpoints may converge before namespace intent selects
their deploy and incumbent endpoints may converge after it stops selecting
them; the join excludes both. A short unavailable interval during convergence
is accepted.

## Runtime identity

Cross-machine deploy RPC uses only the natural managed-replica identity:

```text
(namespace, service, deploy, replica slot)
```

A target node resolves that identity to Docker's local handle immediately
before an effect. Missing or duplicate matches are explicit outcomes. Docker
ids remain private implementation details of the local runtime adapter.

## Consequences

- The separate `services` and `containers` tables are removed.
- A namespace still contains any number of independently named and routed
  services; storage nesting does not merge their identities.
- Deploy inspection, cleanup, logs, and future start/stop commands use node
  RPC, never endpoint testimony.
- Gateway and DNS consume publications rather than fan out to every machine on
  each refresh.
- The controller keeps no service database, container database, plan journal,
  or recovery history.
- An ambiguous namespace publish reply is resolved by rereading that exact
  intent row. This checks whether intent was published, not runtime reality.

This supersedes ADR 0041's Service/Container-row commit and ADR 0042's separate
Service-row and stored-container-key consequences. It narrows ADR 0040's phrase
"Corrosion remains the replicated store": Corrosion stores operator intent and
machine testimony, while Docker remains execution reality and is queried by
RPC whenever an effect decision depends on it.
