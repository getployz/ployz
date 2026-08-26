# Coding standards

Apply to new and touched code. rustfmt and the workspace Clippy lints in `Cargo.toml` remain the source of truth for format and lint. New Clippy suppressions are `#[expect(clippy::lint)]` with a why, not `#[allow]`. Treat `redundant_clone` and `needless_collect` as bugs even when Clippy is quiet.

Run every code change through this lens: ployz updates are cheap; ployzd daemon updates are not. A cluster that opts out of updates can leave ployzd unchanged for years, and every RPC on it is maintenance for as long as it does. Put new behavior in ployz. Latest ployz must still speak to that lagged ployzd — versions eventually drop; that is not the baseline.

Issue #607 is a narrow daemon-policy exception: ployzd owns periodic, Machine-local Global slot convergence because a Global is standing user intent. It may only add missing eligible slots from local Replicated Observations; it never removes or moves slots and never schedules replicated Services. Newer ployz must remain usable against a lagged ployzd that neither converges nor emits Global reconcile observations, so those additive observation fields deserialize absent as empty. This exception does not authorize other daemon-side orchestration.

Issue #616 is a narrow greenfield contract exception: the pre-release Caddy-only daemon and RPC names are replaced in lockstep by the immutable Ingress Proxy Backend contract. Clusters and daemons from before #616 are not migrated or supported through compatibility aliases. This exception does not relax lagged-daemon compatibility for later changes to the new contract.

Runtime Watch's pre-release normalized transport is a narrow greenfield exception: ployz and ployzd switch in lockstep to direct `RuntimeWatchFrame` JSON with negotiated gzip. Pre-change daemon/client pairs are unsupported, and clients derive Services from the transmitted Containers. This exception does not relax lagged-daemon compatibility for later Runtime Watch changes.

## Names

Accessors omit `get_`: `name()`, `as_str()`. Wire RPC method names that already exist on the contract (`get_ingress_proxy_config`) stay as they are.

Conversion prefixes:

- `as_` — cheap borrow
- `to_` — allocates or copies
- `into_` — consumes `self`

Iterators are `iter()` / `iter_mut()` / `into_iter()`. Keep an iterator an iterator until a collection is the result.

## Types

A type cannot represent an illegal state. If two cases cannot both be true, they are not bools, string modes, or paired `Option`s. If two fields must stay consistent, they are one type, not two a caller can desync. Replace those shapes with a type that cannot hold the illegal combination.

A `CONTEXT.md` term is a newtype, not `String` or a primitive. `MachineId` and `MachineName` are different types.

Fallible construction is `parse` → `Result`. Cheap views are `as_str`. Shadow the binding across a transform: `let value = value.parse()?`.

Name a lifetime for the borrow (`'store`, `'src`) when the role is known.

Parameters borrow: `&str`, `&[T]`, `&T`. Clone only when the callee must own. A clone that exists to satisfy the borrow checker is a structure problem. Small `Copy` values pass by value.

A method that is only legal in some states lives on a type that only exists in that state. Observer-relative phases in `CONTEXT.md` stay enums: they are runtime facts.

## Errors

Library and daemon errors are `thiserror` types. The CLI maps those into `Failure`.

Recoverable failures use `?`. Early return without the error value is `let Ok(x) = ... else { return ... }`. `expect("why this is a programmer bug")` is for invariants. `unwrap` belongs in tests.

A fallible public function has an error-path test at its seam.

## Async

Async is for I/O. CPU-bound work stays sync.

Drop a `std::sync` lock before `.await`. Use `tokio::sync::Mutex` when the guard must live across an await.

## Dispatch

Generics (`T: Trait` / `impl Trait`) until a mixed collection or object-safe trait needs `dyn`. Prefer `&dyn Trait` when the callee does not need ownership. Own the `Box`/`Arc` at that boundary, not inside the module.

## Macros

Reuse the existing newtype and RPC macros for repeated mechanical shapes. A one-off stays a function or generic.

## Docs

`//!` on crate and module files. `///` on public items: what it does, and `# Errors` when it returns `Result`. `//` explains why (workaround, design), not what the next line does. TODOs carry an issue id: `// TODO(UT-123):`.
