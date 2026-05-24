# NATS

Historical note: NATS is no longer the target control-plane substrate for new
work. Current distributed-state work targets Corrosion rows/subscriptions plus
bounded iroh peer RPC. Keep this document only for understanding old code or
old design discussions.

Implementation references:

- Assets and replica policy: `crates/ployz-nats/src/buckets.rs`
- Subjects and domains: `crates/ployz-nats/src/subjects.rs`
- Local server config: `crates/ployz-nats/src/config.rs`

Rules:

- Authority, region, HA, and DR are Ployz concepts.
- Streams, KV buckets, leaf nodes, gateways, mirrors, and JetStream domains are
  NATS mechanisms.
- Remote mutations never queue. No responder or timeout is a foreground
  failure.
- Mirrors are async copies, not co-authorities.
