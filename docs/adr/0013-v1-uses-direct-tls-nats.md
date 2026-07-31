# v1 Uses Direct TLS NATS

**Superseded by [ADR 0040](0040-corrosion-replaces-the-core-and-nats.md).**

Ployz v1 machine connectivity uses a direct TLS-authenticated NATS connection to the control plane instead of the previously planned iroh-backed NATS tunnel. iroh remains a possible future private-connectivity option, but v1 removes tunnel bootstrap, tunnel health, tunnel credentials, and tunnel substrate updates so Host Runner and substrate update behavior can stay small and auditable. NATS is still the control-plane authority surface, protected by TLS, authentication, and subject-level permissions.

