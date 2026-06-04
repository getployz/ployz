# NATS Control Plane

Ployz uses NATS as the control-plane backplane and iroh as the private
transport underlay for NATS client connectivity.

## Shape

```text
CLI / SDK / Cloud
  -> NATS services
  -> operation workers
  -> node services
  -> Docker / gateway / DNS / local machine reality
```

Edge nodes connect through a local tunnel:

```text
async-nats
  -> local loopback tunnel
  -> iroh stream
  -> core tunnel endpoint
  -> nats-server
```

The tunnel is byte transport only. Product commands are NATS services.

## Ownership

- `ployz-core`: domain models and product policy.
- `ployz-nats`: NATS resources and API wrappers.
- `ployz-transport`: iroh endpoint identity, join bundles, and NATS byte
  tunnels.
- `ployzd`: process wiring, service handlers, controllers, node services, and
  runtime adapters.
- `ployzctl`: CLI client.
- `ployz-sdk-types`: public schema/type export surface.

## Rules

- KV is current state.
- Streams are operation timelines and durable job triggers.
- Object Store holds larger control-plane artifacts.
- Docker is execution reality.
- Local node storage is cache/evidence.
- iroh does not carry product commands.
- NATS permissions are authoritative over every transport.
