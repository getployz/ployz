# NATS

NATS is the control-plane substrate. The product model is documented in
[`authority-roadmap.md`](authority-roadmap.md).

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
