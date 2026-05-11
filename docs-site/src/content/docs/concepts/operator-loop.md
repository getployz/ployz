---
title: The Operator Loop
description: Observe, choose one command, execute, verify, then decide again.
llms:
  summary: Human and agent loop for operating Ployz.
---

Ployz is built around a simple operator loop:

1. Observe current state.
2. Choose one command.
3. Execute bounded foreground work.
4. Verify evidence and live state.
5. Decide the next command.

There is no hidden controller racing ahead of the operator. That is the point:
the cluster stays small enough to reason about, and each operation has a clear
audience.

![Operator loop diagram](/assets/operator-loop.svg)

For humans, this means transparency. For agents, it means the system is
tractable: they can plan, run one command, inspect structured output, and
verify before moving on.
