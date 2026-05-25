# Substrate Integration Tests

These tests prove the Milestone 0/1 substrate spine without the daemon, CLI, or
WireGuard mesh:

- live Corrosion agents and HTTP API
- live iroh + irpc peer preflight
- `MachineMembershipService::add_machine` writing and observing `machines` rows
- iroh identity restart preserving the same endpoint ID from disk

The `ployzd` crate tests add the next layer: the daemon runtime composes the
Polis Corrosion agent, applies and verifies schema through `CorrosionStore`,
starts persisted iroh identity,
reports typed startup state, and shuts down cleanly.

## Prerequisites

Install the pinned Corrosion agent binary:

```bash
just install-corrosion
```

The installer reads `.corrosion-version` and downloads the matching official
release asset from `superfly/corrosion` into `target/tools/bin/corrosion`.
Workspace `corro-client` and `corro-api-types` dependencies use the same
`v1.0.0` git tag.

## Environment

| Variable | Default | Purpose |
| --- | --- | --- |
| `CORROSION_BIN` | `target/tools/bin/corrosion`, then `corrosion` on `PATH` | Path to the Corrosion agent executable |

CI installs the binary with `just install-corrosion` and exports
`CORROSION_BIN`. Local runs can either use `just install-corrosion` or set
`CORROSION_BIN` explicitly.

## Commands

```bash
just install-corrosion
cargo test -p ployzd -- --nocapture
cargo test -p ployz-e2e substrate_spine -- --nocapture
```

Use `just test-all` before pushing changes that touch the substrate harness,
Polis membership schema, or Corrosion store behavior.

## Notes

- Corrosion starts with the Polis membership replication schema. It has nullable
  non-key payload columns so replicated fragments can materialize without
  sentinel defaults. Typed Polis queries only expose complete machine rows.
- Daemon adoption verifies a local ownership marker and the live Corrosion
  database path before reusing an existing agent process.
- The daemon substrate slice is currently a library/runtime test surface. The
  `ployzd` binary fails closed until it owns a real command surface.
