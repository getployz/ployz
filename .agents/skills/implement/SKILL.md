---
name: implement
description: "Implement a piece of work based on a spec or set of tickets."
disable-model-invocation: true
---

Implement the work described by the user in the spec or tickets.

Use /tdd when the user requests test-first development.

Run affected checks after meaningful changes and the applicable final gate once. Reuse passing results until relevant files change.

Once done, follow the applicable review policy in AGENTS.md.

Commit your work to the current branch.
