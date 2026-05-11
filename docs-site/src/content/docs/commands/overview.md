---
title: Command Model
description: How command docs should describe operations.
llms:
  summary: Command documentation rules.
---

Commands are the product surface. A command page should answer:

- What does this operation change?
- What preconditions does it check?
- Where is the commit point?
- What happens if it fails halfway?
- How do I retry?
- How do I verify the result?
- What structured output can an agent rely on?

Ployz docs must keep current executable commands separate from north-star
primitive names. North-star commands explain product direction; current
commands are what users and agents can run today.
