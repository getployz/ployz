# Cloud Relay is tonic/h2 with two credentials

Cloud reaches a private Cluster through a Cloud Relay the Machine dials out to. This binary speaks plaintext HTTP/2; TLS belongs to the terminator in front. `Register` is a held bidi, `Dial` rendezvous with `Attach`, `Open(id)` rides the control stream, and inner bytes stay opaque Machine RPC. Pairing Credential authenticates `Register` and is rejected on `Dial`; Dial Credential authenticates `Dial`.

Yamux, Quinn, and a Unix shortcut for Cloud were rejected so every environment keeps the same Cloud-plus-Relay topology.
