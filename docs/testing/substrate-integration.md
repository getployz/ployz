# Substrate Integration Tests

These tests prove the Milestone 0/1 substrate spine without the daemon, CLI, or
WireGuard mesh:

- live Corrosion agents and HTTP API
- live iroh + irpc peer preflight
- `MachineMembershipService::add_machine` writing and observing `machines` rows
- iroh identity restart preserving the same endpoint ID from disk

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
cargo test -p ployz-e2e substrate_spine -- --nocapture
```

Use `just test-all` before pushing changes that touch the substrate harness,
Polis membership schema, or Corrosion store behavior.

## Notes

- The canonical Polis membership schema remains strict and rejects partial
  rows.
- The replicated e2e startup schema carries defaults required by Corrosion
  v1.0 file-backed schema loading. Product writes still provide every column.
- This slice does not start the daemon, expose a CLI, derive WireGuard peers, or
  create mesh namespace tables.
