---
title: Primitives, Not Policies
description: Why Ployz exposes operations instead of a standing policy engine.
llms:
  summary: Product thesis around explicit primitives.
---

Ployz exposes real operational primitives instead of asking users to assemble a
platform from low-level knobs.

Good primitives are command-shaped:

- add a machine,
- deploy a workload,
- move a workload and its state,
- branch an environment,
- promote or roll back,
- fork a volume,
- remove a machine.

If a user needs a script to compose several Ployz commands into a common
workflow, that workflow is probably a missing primitive.

## What this rejects

Ployz does not store a standing desired-state document and continuously
reconcile toward it. Policy belongs at decision time. The operator decides what
should happen next; Ployz executes that operation and reports what happened.
