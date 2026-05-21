---
title: Slice 018a p2panda Substrate Substitution Investigation
status: completed
completed: 2026-05-18
plan: MVP/slice-018a-p2panda-substitution-investigation-plan.md
---

# Slice 018a p2panda Substrate Substitution Investigation

## Result

Adopt p2panda early for the fact substrate direction.

The investigation found a real fit for `p2panda-core`, `p2panda-store`, and
`p2panda-stream`: they replace generic signed-operation and append-only-log
plumbing that Ployz should not maintain by hand. Keep PloyzBus and business
reducers custom.

The evidence and crate matrix live in
`MVP/design-notes/p2panda-substitution.md`.

## Proof Added

`MVP/p2panda-spike` is an isolated compile-tested spike. It is not production
substrate. It proves:

- signed p2panda operations can represent Ployz facts;
- operations decode into existing `FactCandidate` values;
- same-key conflicts remain visible to projection;
- payloads are addressable by BLAKE3 content hash.

Verification:

```text
cargo test -p mvp-p2panda-spike --lib
```

Result:

```text
3 passed
```

## Follow-On

Insert a dedicated p2panda-backed fact substrate slice before deploy restart
recovery implementation. The deploy restart proof should not harden the current
custom iroh-docs wrapper if the next substrate direction is p2panda-backed
facts.
