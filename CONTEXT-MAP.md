# Ployz context map

Ployz has two domain contexts. Read the glossary for the context you are changing;
read both when working across their boundary.

| Context | Glossary | Scope |
| --- | --- | --- |
| Core (`core/`) | [core/CONTEXT.md](core/CONTEXT.md) | Machines, Cluster observations, bounded Deploy operations, and the Config Store |
| Dashboard (`dashboard/`) | [dashboard/CONTEXT.md](dashboard/CONTEXT.md) | Accounts, billing, GitHub automation, and Cloud's deploy queue |

Cloud observes the Engine and requests operations. The Engine owns runtime
behavior; each Cluster observation remains relative to an Entry Machine. The
Config Store owns authored configuration and its history; Cloud hosts each
Organization's. Cloud never owns runtime truth.

At the boundary:

- Cloud **Server** refers to an Engine **Machine**, with no separate runtime identity.
- Cloud **Managed Volume** refers to an Engine **Provisioned Volume**; both are Server-local and quota-enforced.
- A **Deployment** records one Engine **Deploy**; in Cloud it also waits in Cloud's queue;
  the two are not interchangeable.
- Shared runtime bootstrap terms follow the Engine glossary; the Cloud glossary
  supplies their product language.

Design decisions live in each project's `DESIGN.md`; there is no separate ADR log.
