# NATS Control Plane

Ployz uses NATS as the control-plane backplane. Machines connect to the control
plane through direct TLS-authenticated NATS in v1.

## Shape

```text
CLI / SDK / Cloud
  -> NATS services
  -> operation workers
  -> machine services
  -> Docker / gateway / DNS / local machine reality
```

Machines connect directly to NATS:

```text
async-nats
  -> TLS NATS
  -> nats-server
```

Product commands are NATS services. NATS credentials and subject permissions are
the authority boundary for every caller.

## Ownership

- `ployz-core`: domain models and product policy.
- `ployz-nats`: NATS resources and API wrappers.
- `ployzd`: process wiring, service handlers, controllers, machine services, and
  runtime adapters.
- `ployzctl`: CLI client.
- `ployz-sdk-types`: public schema/type export surface.

## Rules

- KV is current state.
- Streams are operation timelines and durable job triggers.
- Object Store holds larger control-plane artifacts.
- Docker is execution reality.
- Local machine storage is cache/evidence.
- Private overlay transport is deferred from v1.
- NATS permissions are authoritative over every transport.
