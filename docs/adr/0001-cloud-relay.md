# Cloud Relay is HTTP/1.1 WebSocket behind a TLS terminator

Cloud reaches a private Cluster through a Cloud Relay the Machine dials out to. The process speaks HTTP/1.1: `Register`, `Dial`, and `Attach` are WebSockets; `List` and `Revoke` are POST. TLS belongs to the terminator; an `https` Cloud Pairing `relayUrl` becomes `wss` at the client. `Register` is a held bidi, `Dial` rendezvous with `Attach`, `Open(id)` rides the control stream, and inner bytes stay opaque Machine RPC. Pairing Credential authenticates `Register` and is rejected on `Dial`; the process Dial Credential authenticates `Dial`, `List`, and `Revoke`. Slots are `(pairing, machineId)` ([0002](0002-relay-tenant-slots.md)). GOAWAY is WebSocket close and HTTP 503, not HTTP/2 GOAWAY.

Yamux, Quinn, a Unix shortcut for Cloud, and tonic/gRPC on the public wire were rejected so every environment keeps the same Cloud-plus-Relay topology on commodity HTTPS edges.
