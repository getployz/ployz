# Coding standards

Apply to new and touched code. rustfmt and the workspace Clippy lints in `Cargo.toml` remain the source of truth for format and lint. New Clippy suppressions are `#[expect(clippy::lint)]` with a why, not `#[allow]`. Treat `redundant_clone` and `needless_collect` as bugs even when Clippy is quiet.

Run every code change through this lens: ployz updates are cheap; ployzd daemon updates are not. A cluster that opts out of updates can leave ployzd unchanged for years, and every RPC on it is maintenance for as long as it does. Put new behavior in ployz. Latest ployz must still speak to that lagged ployzd — versions eventually drop; that is not the baseline.

Issues #607 and #613 are narrow daemon-policy exceptions: ployzd owns periodic, Machine-local Global slot convergence because a Global is standing user intent. From local Replicated Observations it may ensure missing known-eligible slots, leave unknown eligibility unchanged for retry, and retire definitely ineligible existing slots; it never moves eligible slots or schedules replicated Services. Issue #613 also authorizes ployzd to enforce mounted Provisioned Volume readiness at every local Container creation entry path. Newer ployz must remain usable against a lagged ployzd that neither enforces this storage policy nor converges or emits Global reconcile observations, so new observation fields deserialize absent as empty. These exceptions do not authorize other daemon-side orchestration.

Issue #664 extends that narrow Machine-local safety boundary: before any Service Container or hook mutation, ployzd may reassess the complete Resolved Service placement against fresh local evidence, ensure mounted Volumes, and reject ordinary mutations that are ineligible or unknown. Observer-side eligibility is a separate advisory consumer decision, including an Unknown safe hold; a dispatched target still performs exactly one fresh authoritative Global convergence decision, which may ensure eligible slots, retire definitely ineligible slots, or hold unknown slots. This remains admission for an already-selected target, not daemon-side scheduling, and latest ployz remains usable against lagged daemons that do not enforce it.

Issue #701 is a narrow greenfield serving-definition change: ployz-core, ployz, and ployzd switch in lockstep so Serving Containers are generation-selected. Pre-change daemons are unsupported. This exception does not relax lagged-daemon compatibility for later serving changes.

Issue #616 is a narrow greenfield contract exception: the pre-release Caddy-only daemon and RPC names are replaced in lockstep by the immutable Ingress Proxy Backend contract. Clusters and daemons from before #616 are not migrated or supported through compatibility aliases. This exception does not relax lagged-daemon compatibility for later changes to the new contract.

Issue #662 is a narrow greenfield contract exception: the pre-release combined named Service Volume source is replaced in lockstep by distinct External, Ordinary, and Provisioned source forms. Clients and daemons from before #662 are not migrated or supported through compatibility aliases. This exception does not relax lagged-daemon compatibility for later changes to the new contract.

Issue #666 is a narrow greenfield contract exception: ployzd closes an exec session when Docker reports that the remote process exited, even if the hijacked stream remains open. Pre-change daemons are unsupported. This exception does not relax lagged-daemon compatibility for later changes.

Issue #668 is a narrow greenfield contract exception: the Ployz-owned ports move from `51000`/`51001`/`51002`/`51500` to `7569`/`7570`/`7571`/`7572`, with client, daemon, and testkit switching in lockstep and beta Machines re-initializing. This exception does not relax lagged-daemon compatibility for later changes.

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
