---
title: Intent, Status, and Observation
description: Separate durable truth from live observation.
llms:
  summary: State model and liveness distinction.
---

Ployz separates three kinds of information:

| Kind | Meaning |
| --- | --- |
| Intent | What an operator explicitly asked the cluster to do. |
| Status | Durable lifecycle facts emitted by operations. |
| Observation | Live reachability, health, capacity, and freshness checked at decision time. |

Durable state should not infer liveness. Observations may be cached for
diagnostics, but they do not silently become cluster policy.

This distinction matters for failure. A stale observation should not rewrite
cluster truth. A failed operation should remain visible until a later operation
resolves it.
