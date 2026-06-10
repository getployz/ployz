# v1 Uses Direct TLS NATS

Ployz v1 machine connectivity uses a direct TLS-authenticated NATS connection to the control plane instead of the previously planned iroh-backed NATS tunnel. iroh remains a possible future private-connectivity option, but v1 removes tunnel bootstrap, tunnel health, tunnel credentials, and tunnel substrate updates so keeper and substrate update behavior can stay small and auditable. NATS is still the control-plane authority surface, protected by TLS, authentication, and subject-level permissions.

