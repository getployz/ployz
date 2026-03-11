# Project Direction

- Read `VISION.md` before making architectural or product-level decisions.
- Treat `VISION.md` as the source for repo scope, product direction, and design
  intent.
- This repo's focus is the orchestrator core, daemon, runtime model, and SDK
  and API surface. Future cloud products are downstream consumers of that core,
  not the source of truth for it.

# Defensive Rust Rules

- Use slice patterns over indexing: `let [a, b] = slice else { ... }` not `slice[0]`
- Use explicit enum values, never `Default::default()`
- Destructure in trait impls to catch new fields: `let Self(x) = self;`
- Never wildcard on project-defined enums — spell out all variants
- Never `.unwrap()` on Option state — use `let Some(x) = opt else { return err }`
- Add `#[must_use]` on all builder methods returning `Self`
- Prefer enums over boolean parameters
