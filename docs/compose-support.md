# Compose Support

Ployz v2 does not ship a Docker Compose input adapter.

The current deploy API creates or updates the sole service in a namespace from
a prebuilt registry image and an explicit Ployz runtime request. It does not
parse Compose files, build source code, deploy multiple services together, or
implement Compose dependency and lifecycle semantics.

[ADR 0007](adr/0007-compose-input-is-an-adapter-not-the-core-model.md) records
the boundary for a possible future adapter; it is not a statement of current
support.
