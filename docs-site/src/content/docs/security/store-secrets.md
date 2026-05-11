---
title: Store Secrets
description: How to think about secrets in the replicated store.
llms:
  summary: Secret material in NATS-backed state.
---

NATS is the native cluster state substrate. Anything written to replicated
streams or key-value buckets must be treated as cluster-private material.

Docs for secret-bearing operations should answer:

- Which machine can read this value?
- Is the value replicated?
- How is authority changed?
- What happens if a participant is missing?
- How does an operator verify the current state?

Logs are not an audience for secret failures. Security-sensitive failures need
structured responses and operator-visible state.
