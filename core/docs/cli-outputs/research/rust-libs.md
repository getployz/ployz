# Rust crates for one ployz output system

The question: which crates should one `ployz::ui` module build on? It has to cover tables, success lines, progress, prompts, warnings and errors, each in three modes: interactive (TTY), plain (piped or CI) and JSON.

Sources, gathered 2026-10-05:

- crates.io API: `/api/v1/crates/<name>`, `/owners`, `/<version>/dependencies`.
- Crate source unpacked from `static.crates.io`: indicatif 0.18.6, console 0.16.6, dialoguer 0.12.0, inquire 0.9.4, cliclack 0.5.6, comfy-table 8.0.1, anstream 1.0.0, anstyle-query 1.1.5.
- uv `main`: `crates/uv/src/printer.rs`, `crates/uv/src/commands/reporters.rs`, `crates/uv-errors/src/lib.rs`, `crates/uv-warnings/src/lib.rs`. Also uv tag `0.4.0`.
- `core/Cargo.lock`, and `cargo tree` on scratch crates.

"New crates" counts the packages a crate would add that `core/Cargo.lock` does not already hold. To get it, I put the crate in an empty project, listed its normal dependency tree and subtracted the 526 names in our lock. Our toolchain is 1.97.1, so every MSRV below is satisfied.

## What we already compile

| Crate | Version | Pulled in by |
|---|---|---|
| anstyle, anstream, anstyle-query, anstyle-parse, colorchoice | 1.0.14, 1.0.0, 1.1.5, 1.0.0, 1.0.5 | clap 4.6.6 (`color` feature, on by default) |
| crossterm | 0.29.0 | ployz directly: deploy redraw, raw-mode prompt, `Stylize` in `deploy/report.rs` |
| unicode-width, unicode-segmentation | 0.2.2, 1.13.3 | ployz directly: `deploy/apply.rs` `terminal_rows` |
| nu-ansi-term | 0.50.3 | in the lock, but not in ployz's normal tree |
| termcolor | 1.4.1 | ts-rs-macros (a proc macro, so not in the binary) |
| shell-words, strsim, encode_unicode | | already present; dialoguer and console reuse them |

Today ployz has three hand-made pieces of output plumbing:

- `Ink`: a TTY and `NO_COLOR` check plus crossterm `Stylize`. It ignores `CLICOLOR`, `CLICOLOR_FORCE` and `TERM=dumb`.
- A cursor-up redraw for the deploy progress view. It handles line wrap but not terminal height.
- A crossterm raw-mode line reader for the data-loss confirmation.

There is no `--color` flag. About 246 `say!`/`say_inline!` call sites and 20 direct `eprintln!` sites would move onto the new module.

## Styling and color detection

| Crate | Downloads (90d) | Last release | MSRV | Owners | New crates |
|---|---|---|---|---|---|
| **anstyle** + **anstream** | 788M / 729M (207M / 191M) | 1.0.14 2026-03-13; 1.0.0 2026-02-11 | 1.66 | epage, rust-cli | **0** |
| owo-colors | 174M (38M) | 4.4.0 2026-08-27 | 1.83 | sunshowers, jam1garner | 1 |
| console | 368M (85M) | 0.16.6 2026-09-10 | 1.71 | mitsuhiko, djc | 1 (comes with indicatif anyway) |
| colored | 241M (47M) | 3.1.1 2026-01-16 | 1.80 | mackwic +2 | 0 to 1 |
| yansi | 272M (57M) | 1.0.1 2024-03-13 | 1.63 | SergioBenitez | 1 |
| nu-ansi-term | 559M (141M) | 0.50.3 2025-10-10 | 1.62 | nushell | 0 (already in the lock) |

How each crate decides whether to use color, from its source:

- **anstream** `AutoStream::choice`, in order:
  1. `NO_COLOR` set: never.
  2. `CLICOLOR_FORCE` set: always.
  3. `CLICOLOR=0`: never.
  4. Otherwise color only on a TTY with a color-capable `TERM` (not `dumb`), `CLICOLOR` set, or CI detected.

  `ColorChoice::write_global` lets one `--color` flag override all of it. `AutoStream` also strips ANSI from anything written to a non-color stream. A stray styled string therefore can't leak escape codes into a pipe.
- **console**: honors `NO_COLOR`, `CLICOLOR`, `CLICOLOR_FORCE` and `TERM=dumb`, with one global switch per stream. It has no CI detection and no stripping writer.
- **owo-colors**: no detection by default. The `supports-colors` feature adds the `supports-color` crate.
- **colored**, **yansi**, **nu-ansi-term**: global or manual switches. None of them strips ANSI on write.

Pick **anstyle + anstream**. They add nothing to the build. They have the most complete env handling. clap's help and its `error:` output already use anstyle, so one palette (`clap::builder::Styles`) can style both clap and ployz. anstyle's `Style` prints as `{style}text{style:#}`, which is enough for a small set of named tones. uv uses anstream for its streams and owo-colors for `.bold().cyan()` convenience. ployz doesn't need that convenience if all styling sits in one `Tone` enum.

## Tables

| Crate | Downloads (90d) | Last release | MSRV | Owners | New crates |
|---|---|---|---|---|---|
| comfy-table | 104M (20M) | 8.0.1 2026-09-25 | 1.88 | Nukesor | 1 (`--no-default-features`; the default `tty` reuses our crossterm 0.29) |
| tabled | 42M (10M) | 0.22.0 2026-09-05 | none set | zhiburt | 5 (papergrid, bytecount, tabled_derive, testing_table, tabled) |
| prettytable-rs | 24M (4M) | 0.10.0 2022-12-27 | none set | phsym | 8 (`term`, `csv`, `dirs-next`, `is-terminal`, …) |
| hand-rolled on unicode-width | already compiled | | | | 0 |

- **comfy-table** is the strongest crate. Its `NOTHING` preset gives borderless columns. `ContentArrangement::Dynamic` fits columns to the terminal width by wrapping cells. `force_no_tty` and `set_width` make output testable. Two drawbacks:
  - Measuring styled cells needs the `custom_styling` feature, which adds `ansi-str`.
  - Its fitting wraps cell text onto extra lines. That makes multi-line rows, which can't be grepped, and kubectl and docker don't do it.
- **tabled** is derive-oriented and the heaviest option (a 233 KB crate). It is built for boxed tables, not borderless CLI lists.
- **prettytable-rs** is stale (no release since 2022) and heavy. Reject it.

Hand-roll it. The table ployz wants:

- **TTY:** borderless, space-aligned, bold header. A status column carries a tone. The current row gets a mark. Overflow is our policy: drop low-priority columns first, then cut the widest cell with `…`.
- **Plain:** TSV with an UPPERCASE header and no padding, so `cut -f` works.
- **JSON:** the rows go into the result.

Padding is about 40 lines on top of `unicode-width` and `unicode-segmentation`, which `deploy/apply.rs` already uses. Styling is easy if cells are typed (`Cell { text, tone }`) rather than pre-styled strings: pad the plain text, then paint it. That needs no ANSI measuring. If we ever want wrapped, width-fitted cells, comfy-table with `--no-default-features` is the drop-in replacement at a cost of one crate.

## Progress

| Crate | Downloads (90d) | Last release | MSRV | Owners | New crates |
|---|---|---|---|---|---|
| **indicatif** | 229M (50M) | 0.18.6 2026-07-01 | 1.85 | mitsuhiko, djc | 4 (indicatif, console, encode_unicode, unit-prefix) |
| kdam | 0.75M (0.1M) | 0.6.4 2026-01-06 | none set | clitic | 2 (adds `terminal_size`) |
| linya | 0.06M | 0.3.1 2024-09-14 | none set | fosskers | 2 |
| status-line | 0.8M | 0.2.0 2021-12-13 | none set | pkolaczk | small, dormant |

What indicatif 0.18.6 does, from its source:

- **Off a TTY it draws nothing.** For a `Term` target, `is_hidden()` is `!term.is_term()`, and the draw path skips when `!term.is_term() || is_dumb()`. `MultiProgress::println` "will not do anything" when the target is hidden. So plain mode has to be written by us. indicatif never produces log lines for CI.
- **It clips to the terminal height.** The multi draw counts wrapped visual lines (`visual_line_count`) and stops before `term.height()`. Our redraw in `deploy/apply.rs` counts wrapped rows, but it emits `\x1b[{n}F` with no height clamp. A deploy with more rows than the screen has lines would leave stale frames behind.
- **It rate-limits redraws** (20 per second on `ProgressDrawTarget::stderr()`), keeps spinners moving with `enable_steady_tick`, and has `MultiProgress::suspend` to print or prompt above live bars.
- **It has a test terminal.** `TermLike` is a trait. The `in_memory` feature adds `InMemoryTerm`, a vt100-backed terminal for testing redraws.

How uv uses it:

- uv's `Printer::target()` returns `ProgressDrawTarget::hidden()` when quiet, verbose or `--no-progress`, else `ProgressDrawTarget::stderr()`.
- Every reporter builds bars under one `MultiProgress::with_draw_target(printer.target())`. A bar is a styled `{wide_msg}` line (`"   Building …"`, `"      Built …"`) or `{bar:20} [{pos}/{len}] {wide_msg:.dim}`.
- When `multi_progress.is_hidden()`, the reporter writes the same start and finish messages as ordinary stderr lines with `writeln!(printer.stderr(), …)`.

That last point is the pattern ployz should copy. One event drives both renderers: indicatif redraws on a TTY, and plain mode writes one line per state transition.

Pick **indicatif**, with `default-features = false, features = ["unicode-width"]` as uv does. In TTY mode it replaces the hand-rolled redraw. Plain mode stays ours, using the existing de-dup idea (`progress_signature`): print only state changes, one line per Server and Service, plus a heartbeat every 30 seconds or so. CI then sees the command is alive during long waits. kdam is tqdm-style and lightly used. linya and status-line are dormant.

## Prompts

| Crate | Downloads (90d) | Last release | MSRV | Owners | New crates | Without a TTY |
|---|---|---|---|---|---|---|
| **dialoguer** | 86M (17M) | 0.12.0 2025-08-23 | 1.66 | mitsuhiko, djc, pksunkara | 1 next to indicatif (`--no-default-features`) | `io::ErrorKind::NotConnected` "not a terminal" from every prompt |
| inquire | 22M (5M) | 0.9.4 2026-02-24 | 1.80 | mikaelmello | 3 (inquire, dyn-clone, fuzzy-matcher; it reuses our crossterm 0.29) | `InquireError::NotTTY` |
| cliclack | 3.7M (1.2M) | 0.5.6 2026-08-10 | none set | fadeevab | 11 (console, indicatif, textwrap, icu_segmenter, …) | prompts return `NotConnected` |
| promptly | 3M | 0.3.1 2022-06-08 | none set | anowell | small | stale |

All three live crates fail cleanly without a TTY. ployz should still decide before calling any of them, for two reasons:

- `--json` never prompts.
- A choice without a terminal must fail with `invalid_argument` naming the flag that settles it.

So `ui::confirm` checks the mode and never surfaces the library's error.

Pick **dialoguer** with `default-features = false`. It shares indicatif's `console::Term`, so a prompt can run inside `MultiProgress::suspend`. It covers what ployz asks for:

- `Input` with `validate_with`, for type-the-name-to-confirm data loss.
- `Select`, for the context picker.
- `Confirm`.

It prompts on stderr by default. `interact_on(&Term::stdout())` keeps the current "human text on stdout" rule if we keep it. inquire is a fine alternative with richer autocomplete, at two more crates.

cliclack is a whole-UI kit: `intro`, `outro`, `log::*`, spinners and prompts in the `│ ◇ └` clack style. Its `term_write` writes those gutter characters to `Term::stderr()` without checking for a TTY. Logs and CI output would carry the box art, and it has a single global theme. It's a good visual reference for interactive mode, but not a foundation.

## Errors

| Crate | Downloads (90d) | Last release | MSRV | Owners | New crates | Fits ployz? |
|---|---|---|---|---|---|---|
| miette | 79M (19M) | 7.6.0 2025-04-27 | 1.70 | zkat | 2; 18 with `fancy` (backtrace, gimli, textwrap, icu, supports-*) | Built for source-span diagnostics. ployz errors are RPC and runtime failures with `code`, `details.next` and `details.valid_children`, not spans. |
| annotate-snippets | 55M (19M) | 0.12.16 2026-05-06 | 1.85 | rust-lang | 1 (uses anstyle) | Only useful if ployz someday points into a config file, `ployz.yaml`, at line and column. |
| color-eyre | 72M (14M) | 0.6.5 2025-05-30 | 1.65 | yaahc, eyre-rs | 11 (backtrace stack, tracing-error) | An app panic and report hook. It doesn't render product errors. |

How uv renders errors without miette (`uv-errors::write_error_chain_with_options`):

```
error: <top Display, wrapped to width, continuation indented under the text>
  cause: <source() #1>
         <continuation lines indented 9>
  cause: <source() #2>

hint: <each hint, blank line before>
```

`error`, `warning` and `cause` are bold in the level's color. `hint` is bold cyan. Warnings use the same function with `level("warning")` and yellow, and `warn_user_once!` de-duplicates on the fully rendered text. uv 0.4.0 printed `  Caused by: <err>` in bold red instead, and cargo still uses a `Caused by:` block.

Hand-roll it the uv way, in about 40 lines with anstyle. Walk `Error::source()`, then print the hints that `failure.rs` already derives from `details` (`next`, `valid_children`). One prerequisite: today ployz flattens causes into the top message (for example `could not reach Cloud at …: error sending request: client error (Connect): …`). For the chain to read cleanly, the `thiserror` types must stop putting `{source}` in their `#[error]` text. The renderer prints the cause on its own line instead. JSON keeps the full text in `message`, so nothing is lost.

## Recommended stack

| Concern | Choice | New crates | Why |
|---|---|---|---|
| Color and env detection | **anstyle + anstream** | 0 | Already compiled through clap. Handles `NO_COLOR`, `CLICOLOR`, `CLICOLOR_FORCE`, `TERM=dumb` and CI. Strips ANSI on pipes. One global `--color` override. The same palette styles clap help. |
| Tables | **hand-rolled** on unicode-width + unicode-segmentation | 0 | Borderless aligned columns on a TTY and TSV when piped, with our own overflow policy. Typed cells avoid ANSI measuring. comfy-table (+1) is the fallback. |
| Progress | **indicatif** (`default-features = false, features = ["unicode-width"]`) | 4 | TTY redraw with height clipping, rate limiting, spinners, `suspend` and an in-memory test terminal. Plain mode stays ours and is driven by the same events. This is uv's pattern. |
| Prompts | **dialoguer** (`default-features = false`) | 1 | Same `console::Term` as indicatif. Fails cleanly without a TTY. `ui` refuses before calling it. |
| Errors and warnings | **hand-rolled**, uv-style `error:` / `cause:` / `hint:` | 0 | ployz errors carry codes and details, not spans. miette, annotate-snippets and color-eyre solve other problems. |
| Kit | none | | cliclack's look doesn't degrade for logs, and it adds 11 crates. The console-rs family (console, indicatif, dialoguer) is the de facto kit and is what we take, minus `console::style`. |

Total: 4 new crates (indicatif, console, unit-prefix, dialoguer), all maintained by mitsuhiko and djc. crossterm stays for `operator.rs` raw mode. It can leave `deploy/report.rs` (styling) and `data_loss.rs` (the prompt).

## Sketch: `ployz::ui`

Handlers pass values and typed parts, never strings with layout in them. Under `--json`, the value is the result. The human rendering is derived from the same value, so "anything human output reports is also in the JSON result" holds by construction.

```rust
//! Every byte a person sees goes through here; handlers never format output by hand.
pub enum Mode { Interactive, Plain, Json }            // resolved once, before dispatch
pub fn init(json: bool, color: clap::ColorChoice);    // sets anstream's global choice and the Mode
pub fn mode() -> Mode;

pub enum Tone { Plain, Muted, Name, Good, Busy, Bad }   // the whole palette; anstyle behind it
pub struct Cell<'a> { pub text: Cow<'a, str>, pub tone: Tone }
pub struct Column<T> { pub header: &'static str, pub cell: fn(&T) -> Cell<'_>, pub keep: Keep }
pub enum Keep { Always, IfRoom }                      // TTY overflow drops IfRoom columns first
pub struct Line { /* sentence built from typed parts */ }
impl Line {
    pub fn new(text: &str) -> Self;
    pub fn name(self, name: &dyn Display) -> Self;    // a domain name, toned Tone::Name
    pub fn text(self, text: &str) -> Self;
}
pub enum Hint { Next(Command), Valid(Vec<String>), Undo(Command) }  // one vocabulary

// Results: exactly one per command.
pub fn done<T: Serialize>(result: &T, line: Line, hints: &[Hint]) -> Result<(), Failure>;
pub fn list<T: Serialize>(noun: &str, rows: &[T], columns: &[Column<T>], empty: Line) -> Result<(), Failure>;
pub fn fields<T: Serialize>(noun: &str, value: &T, fields: &[(&str, Cell<'_>)]) -> Result<(), Failure>;
pub fn stream<T: Serialize>(item: &T, line: Line) -> Result<(), Failure>;  // NDJSON or one human line

// Side channel: stderr in every mode; warnings also land in the JSON result's `warnings`.
pub fn warn(line: Line);
pub fn note(line: Line);                              // human-only context, never in JSON

// Progress: indicatif redraw on a TTY, one line per transition in Plain, stderr under Json.
pub fn progress(title: Line, total: usize) -> Progress;
impl Progress {
    pub fn task(&self, server: &MachineName, label: &str) -> Task;
    pub fn println(&self, line: Line);                // above the bars, or a plain line
    pub fn finish(self, outcome: Line, hints: &[Hint]);
}
pub enum TaskState { Pending, Running, Done, Unchanged, Failed }
impl Task { pub fn set(&self, state: TaskState, detail: Option<&str>); }

// Prompts: Interactive only; otherwise Err(invalid_argument) naming `settle`.
pub fn confirm_name(loss: Line, expected: &str, settle: Flag) -> Result<(), Failure>;
pub fn select<'a, T>(question: &str, options: &'a [T], label: fn(&T) -> Cow<'_, str>, settle: Flag) -> Result<&'a T, Failure>;

// Errors: replaces failure::terminate. error:/cause:/hint: lines, or {"error":…}.
pub fn exit(result: Result<(), Failure>) -> ExitCode;
```

How each function behaves in each mode:

| Function | Interactive | Plain | Json |
|---|---|---|---|
| `done` | toned sentence, then hint lines | the same sentence and hints, no ANSI | the result object, with `next` from `hints` |
| `list` | aligned columns, bold header, empty sentence | UPPERCASE TSV header and rows; empty prints header plus sentence | `{noun: rows}` |
| `fields` | aligned `key  value` | `key = value` | `{noun: value}` |
| `warn` | yellow `warning:` on stderr | `warning:` on stderr | stderr, plus `warnings` in the result |
| `progress` | indicatif `MultiProgress` | one line per state change, plus a heartbeat | lines on stderr (or `--events` NDJSON) |
| `confirm_name`, `select` | dialoguer | `invalid_argument` with `settle` | `invalid_argument` with `settle` |
| `exit` | `error:`, `cause:`, `hint:` | the same, no ANSI | `{"error":{code,message,details}}` |

Tests swap the sink, as `output::captured` does today. Each mode then renders to a buffer and gets asserted as text, and indicatif's `InMemoryTerm` covers the TTY redraw.
