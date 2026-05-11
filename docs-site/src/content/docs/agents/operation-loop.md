---
title: Agent Operation Loop
description: The repeatable plan-execute-verify loop for agents.
llms:
  summary: Stepwise loop for agents.
---

An agent should operate Ployz in a tight loop:

1. Observe current state.
2. Choose one explicit command.
3. Execute the foreground operation.
4. Inspect structured result and evidence.
5. Verify live state.
6. Decide the next operation.

Avoid batching speculative mutations. Ployz intentionally makes operations
small enough to inspect and reason about one at a time.
