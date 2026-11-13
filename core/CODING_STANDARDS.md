# Coding standards

Apply to new and touched code.

Architecture and the current compatibility policy live in [DESIGN.md](DESIGN.md).

## Types

A type cannot represent an illegal state. If two cases cannot both be true, they are not bools, string modes, or paired `Option`s. If two fields must stay consistent, they are one type, not two a caller can desync. Replace those shapes with a type that cannot hold the illegal combination.

A `CONTEXT.md` term is a newtype, not `String` or a primitive. `MachineId` and `MachineName` are different types.

A method that is only legal in some states lives on a type that only exists in that state. Observer-relative phases in `CONTEXT.md` stay enums: they are runtime facts.

## Errors

Library and daemon errors are `thiserror` types. The CLI maps those into `Failure`.

## CLI output

Every command that produces a result takes `--json` and prints it through `crate::ui`: one JSON object on stdout, keyed by noun (`{"servers": […]}`). Streams print one object per line. Anything human output reports is also in the JSON result. A mutation emits its outcome even when nothing changed. A command that cannot produce one JSON result refuses `--json` in the handler table; it never ignores the flag. `--json` never prompts: a choice that would prompt takes its documented default or fails with `invalid_argument` naming the flag that settles it.

The result goes to stdout and everything else to stderr. Finish with `ui::list` (a `Table`), `ui::fields` (a `Fields` record), `ui::done` (a sentence) or `ui::finish`; side text goes through `ui::note`, `ui::warn`, `ui::warn_cause` and `ui::hint`. A fan-out finishes with `ui::finish_fanout`, whose `Gaps` warns once per unanswered Server by name and exits 3. Human output names Servers and shows Deployments as `#N`; an ID appears only to tell apart rows that would otherwise read the same, or where the ID is what the next command takes. A success `next:` hint appears only when the next step isn't obvious.

A fan-out result carries its per-Machine `failures` and `omitted`; a per-Machine `not_found` is not a failure. A fan-out answers `not_found` only when every Machine answered; otherwise it prints the partial result (a null value plus `failures`/`omitted`). A printed result with a part missing exits 3; a printed result that is itself a failure, such as a failed Deployment, exits by its code (`Failure::outcome`). A failure before any result prints `{"error": {code, message, details, cause}}` in the RPC error vocabulary. Give a `Failure` its real code (`not_found`, `ambiguous`, `conflict`, `unavailable`); `usage` means the input was wrong.

The exit code comes from the code, through the one table in `ui::exit_code`: `invalid_argument`, `confirmation_required` and `ambiguous` exit 2, every other failure (`unsupported` included) exits 1. Exit 3 means only a partial result: part of what printed is missing because a Server failed or didn't answer, or the output stopped partway. Handlers never pick an exit code; only `exec` passes its process's code through `Failure::exit`.

A failure's first line is our sentence about the domain thing, capitalized and ending with a period. The raw cause (OS, HTTP, Docker text) is the error's `#[source]`. Human output prints the deepest source on one `cause:` line, and `--json` lists every source in `cause`, outermost first; never interpolate a source into `#[error]`, and never flatten an error with `to_string()` on its way to a `Failure`. Wrap a lower failure with `Failure::context` to keep its code and chain it as the cause.

The command that fixes or follows a failure is a typed hint, `Failure::hint(Hint::Next | Inspect | Retry | Undo | Closest | Valid)`, never prose in the message. Human output prints each hint on its own line: `next:`, `inspect:`, `retry:`, `undo:` and `valid:` lead with their label, and the near-miss prints unlabelled as `did you mean NAME?`. Only `inspect:` can repeat, so `valid:` prints once. JSON carries the hints in `details`.

JSON fields are only added, never renamed or repurposed; before 0.3.0 a field may be renamed or reshaped outright, with no compatibility alias. A short flag has one meaning across the whole tree.

## Decisions

A decision a model checks lives in `ployz-decide` as a pure function; the model calls it, never re-states it. Statement order across processes (the upgrade receipt and marker, the credential temp file and rename, pool ownership, removal marks, volume marks, Cloud's TypeScript walks) stays a model and links to the code by symbol path.

## Docs

A field or variant whose name and type already say what it holds takes no doc; when one has a doc, it says what the type cannot, such as units, who sets it, or what `None` means. `//` explains why (workaround, design), not what the next line does.
