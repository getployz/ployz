---
title: Slice 047 Product Binary State
status: completed
created: 2026-05-19
origin:
  - MVP/slice-047-product-binary-state-plan.md
  - MVP/three-server-product-vertical-plan.md
---

# Slice 047 Product Binary State

## Result

Slice 047 creates `mvp-node`, the first product-facing MVP binary. It is not a
deployable cluster node yet, but it establishes the durable state boundary the
three-server vertical will build on.

The binary currently supports:

- `init --state <dir> [--island <id>] [--node-id <id>]`
- `status --state <dir>`

It persists:

- node/island/principal identity,
- p2panda fact author private key,
- p2panda-net network id, node seed, and topic seed,
- WireGuard overlay identity placeholder.

Fact store, projection DB, gateway snapshot, and DNS snapshot paths are derived
from the requested state directory on load rather than persisted into the JSON
state file. That keeps copied/restored state from writing future facts or
snapshots to stale paths.

Future product commands (`invite`, `join`, `daemon`, `gateway`, `dns`,
`deploy`) are intentionally present only as explicit not-wired errors. That
keeps the CLI shape visible without pretending the three-server behavior exists
before it is wired and tested.

## Proof

The focused crate test proves:

- init creates reopenable durable state,
- status reads the same state after reopen,
- second init refuses to overwrite existing state,
- missing state returns a structured not-initialized error,
- corrupt state returns a structured decode error,
- unsupported state schema versions are rejected at load,
- copied state rehydrates derived paths from the requested state directory,
- concurrent double-init cannot clobber the durable state file,
- persisted p2panda author/network/node/topic values parse back through the
  real substrate types.

## Next Slice

Slice 048 should implement product `invite` and `join` enough for three local
product nodes to converge membership through p2panda-net-backed facts.
