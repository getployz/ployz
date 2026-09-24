# Ployz context map

Ployz has two domain contexts. Read the glossary for the context you are changing;
read both when working across their boundary.

| Context | Glossary | Scope |
| --- | --- | --- |
| Core (`core/`) | [core/CONTEXT.md](core/CONTEXT.md) | Machines, Cluster observations, and bounded Deploy operations |
| Dashboard (`dashboard/`) | [dashboard/CONTEXT.md](dashboard/CONTEXT.md) | Product authoring, Saved State, and Cloud Deployment Attempts |

Cloud observes the Engine and requests operations. The Engine owns runtime
behavior; each Cluster observation remains relative to an Entry Machine. Cloud
owns authored configuration and product workflow history, not runtime truth.

At the boundary:

- Cloud **Server** refers to an Engine **Machine**, with no separate runtime identity.
- A **Cloud Deployment Attempt** includes product steps around an Engine **Deploy**;
  the two are not interchangeable.
- Shared runtime bootstrap terms follow the Engine glossary; the Cloud glossary
  supplies their product language.

Architectural decisions live in [docs/adr/](docs/adr/).
