# Coding standards

Apply to new and touched code. rustfmt and the workspace Clippy lints in `Cargo.toml` remain the source of truth for format and lint. New Clippy suppressions are `#[expect(clippy::lint)]` with a why, not `#[allow]`. Treat `redundant_clone` and `needless_collect` as bugs even when Clippy is quiet.

Architecture and the current compatibility policy live in [DESIGN.md](DESIGN.md).

Tests verify observable behavior or explicit contracts; reject change-detector assertions that merely mirror source text, incidental formatting, constants, or internal call counts.

## Names

Accessors omit `get_`: `name()`, `as_str()`.

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

## CLI output

Every command that produces a result takes `--json` and prints it through `crate::output`: one JSON object on stdout, keyed by noun (`{"servers": […]}`). Streams print one object per line. Anything human output reports is also in the JSON result. A mutation emits its outcome even when nothing changed. A command that cannot produce one JSON result refuses `--json` in the handler table; it never ignores the flag. `--json` never prompts: a choice that would prompt takes its documented default or fails with `invalid_argument` naming the flag that settles it.

Human text goes through `say!`; clippy denies `print_stdout` in the CLI crate.

A fan-out result carries its per-Machine `failures` and `omitted`; a per-Machine `not_found` is not a failure. A fan-out answers `not_found` only when every Machine answered; otherwise it prints the partial result (a null value plus `failures`/`omitted`). A printed result that did not fully succeed exits 3. A failure before any result prints `{"error": {code, message, details}}` in the RPC error vocabulary and exits 1, or 2 for a rejected command line. Give a `Failure` its real code (`not_found`, `ambiguous`, `conflict`, `unavailable`); `usage` means the input was wrong. Map CLI-owned error enums to codes with exhaustive matches over their variants, no `_ =>` arm, so a new variant must choose its code.

JSON fields are only added, never renamed or repurposed. A short flag has one meaning across the whole tree.

## Async

Async is for I/O. CPU-bound work stays sync.

Drop a `std::sync` lock before `.await`. Use `tokio::sync::Mutex` when the guard must live across an await.

## Dispatch

Generics (`T: Trait` / `impl Trait`) until a mixed collection or object-safe trait needs `dyn`. Prefer `&dyn Trait` when the callee does not need ownership. Own the `Box`/`Arc` at that boundary, not inside the module.

## Macros

Reuse the existing newtype and RPC macros for repeated mechanical shapes. A one-off stays a function or generic.

## Docs

`//!` on crate and module files. `///` on public items: what it does, and `# Errors` when it returns `Result`. `//` explains why (workaround, design), not what the next line does. Open work is `// TODO:` plus why. Optional GitHub issue: `// TODO(#123):`.
