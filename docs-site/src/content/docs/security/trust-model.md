---
title: Trust Model
description: Storage authority and control-plane trust boundaries.
llms:
  summary: Security trust boundary.
---

Nodes with `storage=true` are trusted control-plane participants. They may hold
cluster-private material.

Trust-sensitive material can include:

- TLS private keys,
- ACME account keys,
- invite tokens,
- replicated control-plane facts.

:::danger
Do not treat a storage participant as an untrusted worker. Store authority is a
security boundary.
:::

Security docs should make trust visible before users add machines, promote
storage authority, or move cluster-private data.
