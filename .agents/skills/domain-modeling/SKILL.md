---
name: domain-modeling
description: Build and sharpen the domain model in `core/CONTEXT.md` and `dashboard/CONTEXT.md`. Use when discussing codebase terminology or writing or editing a CONTEXT.md.
---

# Domain Modeling

Actively build and sharpen the domain model as you design: challenge terms, invent edge-case scenarios, and write the glossary down the moment a term crystallises. Merely reading a glossary for vocabulary is not this skill.

The repo has two glossaries. `core/CONTEXT.md` holds Engine terms and `dashboard/CONTEXT.md` holds Cloud terms. Work across the boundary touches both. If the topic's context is unclear, ask.

## During the session

### Challenge against the glossary

When the user uses a term that conflicts with the glossary, call it out immediately. "Your glossary defines 'cancellation' as X, but you seem to mean Y. Which is it?"

### Sharpen fuzzy language

When the user uses vague or overloaded terms, propose a precise canonical term. "You're saying 'account': do you mean the Customer or the User? Those are different things."

### Discuss concrete scenarios

Stress-test domain relationships with specific scenarios that probe edge cases and force precision about the boundaries between concepts.

### Cross-reference with code

When the user states how something works, check whether the code agrees. Surface any contradiction: "Your code cancels entire Orders, but you just said partial cancellation is possible. Which is right?"

### Update CONTEXT.md inline

When a term is resolved, update the glossary right there. Don't batch. Use the format in [CONTEXT-FORMAT.md](./CONTEXT-FORMAT.md).

A glossary holds no implementation details. It is not a spec, a scratch pad, or a record of implementation decisions.
