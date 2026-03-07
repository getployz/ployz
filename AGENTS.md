# Defensive Rust Rules

- Use slice patterns over indexing: `let [a, b] = slice else { ... }` not `slice[0]`
- Use explicit enum values, never `Default::default()`
- Destructure in trait impls to catch new fields: `let Self(x) = self;`
- Never wildcard on project-defined enums — spell out all variants
- Never `.unwrap()` on Option state — use `let Some(x) = opt else { return err }`
- Add `#[must_use]` on all builder methods returning `Self`
- Prefer enums over boolean parameters
