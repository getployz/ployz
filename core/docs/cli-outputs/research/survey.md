# CLI output survey: who does it best, and on what

This page surveys how 13 CLIs render output, so the uniform ployz output design can copy proven patterns instead of inventing them. It covers what each is built on, how it renders lists, progress, errors, prompts and machine output, and how it behaves when stdout or stderr is not a terminal. [../README.md](../README.md) describes what ployz prints today.

Sources: shallow clones read on 2026-10-05. Paths below are relative to each repo root.

| Repo | Commit | Repo | Commit |
|---|---|---|---|
| astral-sh/uv | 46b84fd | superfly/flyctl | 95b7f3e |
| rust-lang/cargo | bb2126c | docker/compose | 42f4807 |
| jj-vcs/jj | 4df5265 | basecamp/kamal | fd335d8 |
| railwayapp/cli | 1bc0563 | cloudflare/workers-sdk (wrangler) | f025bbf |
| oven-sh/bun (now Rust) | c7b06d9 | vercel/vercel (packages/cli) | c628be7 |
| cli/cli (gh) | 6fc1c29 | pnpm/pnpm | fca2d32 |
| charmbracelet/lipgloss, huh, gum | 6a419c6, ffb6a97, 879f048 | | |

"(unverified)" marks claims that rest on a library that wasn't read (indicatif internals, go-gh, docker/cli, chalk, SSHKit) or on memory. Everything else was read in source or in a test snapshot.

## Ranking for a deployment CLI

1. **uv.** Best errors and the cleanest stream discipline of any Rust CLI. Every error renders as `error:` plus indented `cause:` lines plus `hint:` lines. Hints attach to error types and are collected by walking the chain. stdout carries only data (lists, trees, JSON). Progress, summaries, warnings and errors go to stderr. When stderr is not a TTY, live bars become append-only lines instead of disappearing. It is built on crates ployz can use directly: anstream, owo-colors and indicatif.
2. **docker compose.** Best progress model, and the closest domain: containers changing state across a set of resources. One event stream feeds four renderers picked by `--progress auto|tty|plain|json|quiet`. `auto` checks **stderr**, so `up | tee` keeps the live view. The TTY view redraws one row per resource with a mark, a status and a timer, and leaves the last frame on screen. The plain view appends one line per event. The JSON view writes NDJSON.
3. **gh.** Best switch between TTY and pipe for lists and prompts. On a TTY, tables have aligned columns, uppercase headers and relative times. Piped, they become TSV with no header and RFC3339 times. `--json fields` plus `--jq` and `--template` give stable machine output. A prompt that can't run fails and names the flag: `--yes required when not running interactively`.

Runners-up:
- **Railway CLI** has the ployz contract almost word for word: stdout holds one JSON object, or an NDJSON stream, or a JSON error with `code` and `hint`. `→ hint` lines appear under errors. It breaks its own stream rule in `railway up`, though.
- **jj** colors by semantic label, not at call sites. It numbers its `Caused by:` chain and makes styling testable with `--color=debug`.
- **pnpm** is one event stream with swappable reporters (`default`, `append-only`, `ndjson`, `silent`). Its Rust port is direct prior art.
- **Vercel** prints the deployment URL alone on stdout when piped.

## Five conventions most worth adopting

1. **stdout is the result, stderr is the conversation.** Lists, the final result and JSON go to stdout. Progress, status, warnings, prompts and human errors go to stderr. Seen in uv, cargo, jj, Vercel and the Railway contract. ployz today puts progress and tables on stdout in human mode.
2. **One event stream, three renderers, chosen once.**
   - The modes are `tty` (multi-line redraw), `plain` (append-only, one line per state change, no ANSI) and `json` (NDJSON events). The final result is a separate object.
   - `auto` picks `tty` only when the stream the progress draws on is a terminal and `TERM` is not `dumb`. CI turns off animation, not progress lines.
   - Seen in compose, pnpm and flyctl's statuslogger.
3. **The annotated error.** The layout is `error: <our sentence>`, then one `  cause: …` per source in the chain, then a blank line, then `hint: <next step>`. Hints are typed and attached to error kinds; they are not baked into the message. JSON carries the same parts. Seen in uv, cargo and jj.
4. **Never prompt without a terminal; fail and name the flag.** For example `--yes required when not running interactively`. Prompts draw on stderr. Seen in gh, flyctl, Railway and jj.
5. **One color decision, by the standard rules.** Precedence: `--color auto|always|never`, then `NO_COLOR`, then `CLICOLOR_FORCE`/`FORCE_COLOR`, then the stream is a TTY and `TERM` is not `dumb`. Decide per stream. All styles come from one semantic palette (error red, warning yellow, hint cyan, done green, detail dim). uv and cargo get this by delegating to `anstream`; jj gets the palette with labels.

## Progress across the three modes

```
                  one stream of events
         (resource id, state, detail, current/total)
                           |
      +--------------------+---------------------+
      |                    |                     |
  TTY renderer        plain renderer        JSON renderer
  stderr, redraws     stderr, appends       NDJSON events
  rows in place:      one line per          (compose: stderr;
  mark, text, timer   state change,         pnpm: stdout);
  first draw delayed  no ANSI, throttled    result object
  last frame kept     no spinner frames     on stdout
```

### Who redraws, who appends

| Tool | TTY | Not a TTY | CI | JSON mode |
|---|---|---|---|---|
| cargo | One-line `\r` bar under append-only status verbs | Status verbs only; the bar is gone | Bar off (`CI`, `TF_BUILD`) | `--message-format=json` on stdout; status still on stderr |
| uv | indicatif `MultiProgress`, several rows redrawn | Appends `Building`/`Built`/`Downloading`/`Downloaded` lines, plus summaries | Not special | JSON on stdout; progress stays on stderr |
| jj | One-line redraw on stderr | Nothing | Not special | None (templates instead) |
| bun | One-line redraw on stderr | No progress; summary only | No progress | None for install |
| gh | Spinner on stderr; needs stdout **and** stderr to be TTYs | Nothing | Not special | Spinner still runs if both are TTYs |
| flyctl | statuslogger: multi-line block, one row per Machine, 50 ms | Appends `> [1/3] text` lines on stdout | Not special | `--json` per command |
| docker compose | `[+] Running 3/3`, one row per resource, 100 ms | One line per event | `auto` decides on stderr only | `--progress json`: NDJSON on stderr |
| Railway | indicatif spinners | `Indexing...` lines on stdout; **skips log streaming** unless CI | CI mode streams build logs | `{"status":…}` objects |
| Vercel | ora spinner after a 300 ms delay; bar redraws | Each spinner message becomes a line; upload bar only at 25% steps | Not special | JSON on stdout |
| wrangler | log-update spinner on **stdout** | `├ msg` once, `│ msg` on stop | Only Pages/Workers CI | NDJSON side file (`WRANGLER_OUTPUT_FILE_PATH`) |
| pnpm | ansi-diff multi-line redraw, 200 ms | Append-only; progress line at most once a second | CI picks append-only | `--reporter=ndjson` |
| kamal | Append-only always (SSHKit lines) | Same | Same | None |
| gum spin | Spinner on stderr | Title only, no spinner | Not special | n/a |

Evidence:
- **cargo:** `src/util/progress.rs:57-67` (gate), 441-455 (500 ms delay, then 100 ms throttle). `crates/cargo-util-terminal/src/shell.rs:109-116` (`NoTty`), 367-379 (`progress_supported`).
- **uv:** `crates/uv/src/printer.rs` (`suppresses_progress`, hidden draw target). `crates/uv/src/commands/reporters.rs:179, 209, 266, 281, 325` (`is_hidden()` fallback to plain lines).
- **jj:** `cli/src/ui.rs:473-487`. `cli/src/progress.rs:30-31, 70-95` (250 ms delay, 30 Hz).
- **bun:** `src/install/PackageManager/PackageManagerOptions.rs:609-619`. `src/bun_core/Progress.rs:425-444, ~507` (on a dumb stream it prints `\n` instead of erasing).
- **gh:** `pkg/iostreams/iostreams.go:304-316` and 545-551. With `GH_SPINNER_DISABLED` on a TTY it prints one cyan `Label...` line on stderr (L343-359). Off a TTY it prints nothing.
- **flyctl:** `internal/statuslogger/create.go:11-55`, `interactivelogger.go`, `noninteractive.go:34-45`.
- **compose:** `cmd/compose/compose.go:706-750`. The comment says stdout is not probed because "probing stdout would force plain mode whenever stdout is redirected (e.g. `docker compose up | tee log`)". Renderers are in `cmd/display/tty.go`, `plain.go` and `json.go`.
- **Railway:** `src/commands/up.rs:207-336`. `src/util/progress.rs` (`UpdateStep` falls back to `  … msg` then `  ✓ msg`).
- **Vercel:** `packages/cli/src/util/output/create-output.ts:174-196`. `src/util/deploy/process-deployment.ts:197-225` (`stepSize = isTTY ? 0 : 0.25`).
- **wrangler:** `packages/cli/interactive.ts:631-732`. `packages/workers-utils/src/is-interactive.ts:40-71`.
- **pnpm:** `pnpm/src/main.ts:257-262` (TS). Rust port: `pnpm/crates/default-reporter/src/lib.rs:274-294`.

### What the evidence says

- **Decide on the stream you draw to.** compose checks stderr alone, gh requires both streams, Railway and wrangler check stdout. Checking stdout means `deploy | tee log` loses the live view for no reason. compose's choice is the right one for a renderer on stderr.
- **Non-TTY behavior falls into three families.**
  - *Silence:* jj, gh, bun, and cargo's bar. Fine for operations that take seconds. Wrong for a deploy that runs for minutes: a CI log with no lines looks hung.
  - *Append one line per state change:* compose plain, uv, flyctl, pnpm (throttled to 1/s), Vercel (25% steps). This is the right family for deploys. The CI log becomes the record.
  - *Structured stream:* compose `--progress json`, pnpm `--reporter=ndjson`, wrangler's NDJSON file. This is the agent and automation channel.
- **Redraw hygiene everyone converged on:**
  - Delay the first frame so fast commands never flicker: cargo 500 ms, bun 500 ms, Vercel 300 ms, jj 250 ms.
  - Throttle the redraw: 30 Hz to 10 Hz.
  - Keep the final frame as the record: compose `Done()`.
  - Clear the live block before writing an interleaved line, then redraw: cargo `needs_clear`, flyctl `Pause()`, pnpm pauses during prompts, compose drops `Started` rows while attached logs stream.
  - Truncate rows to the terminal width with `…`.
- **The rows are resources, not phases.** compose shows one row per container and flyctl one per Machine, each with its own mark, status and elapsed time. That matches a ployz Deployment, which has a list of nodes (`web`, `api`, `pg-data`) with states. Today ployz prints a whole `queued: web pending, api pending, …` line per transition. Both the TTY rows and the plain lines can come from that same event stream.
- **Under JSON, keep progress off stdout.** compose puts progress NDJSON on stderr. uv and cargo keep human status on stderr while JSON goes to stdout. Railway suppresses progress entirely in JSON mode.

## The annotated error, compared

| Tool | Layout | How hints attach | Source |
|---|---|---|---|
| uv | `error:` / `  cause:` per source / blank line / `hint:` lines | `Hinted` trait on error types; `hints_for_error` walks `err.chain()`, downcasts, removes duplicates, orders First/Any/Last | `crates/uv-errors/src/lib.rs:349-390`, `crates/uv/src/commands/diagnostics.rs:54-91` |
| cargo | `error:` / blank / `Caused by:` / 2-space indented causes; `help:` / `note:` | Mostly plain text in the message; annotate-snippets `Level::HELP` for lints | `src/lib.rs:175-248` |
| jj | `Error:` / `Caused by:` then `1:`, `2:` / `Hint:` lines | `Vec<ErrorHint>` on `CommandError`; the kind sets the exit code | `cli/src/command_error.rs:88-161, 1017-1105` |
| Railway | anyhow chain, then `  → hint` | `RailwayError::hint()` and `code()`; JSON `{error, code, hint}` on stdout | `src/util/reporter.rs`, `src/errors.rs` |
| flyctl | `Error: msg` / description / blank / suggestion / `View more information at  <url>` | `flyerr` interfaces: `ErrorDescription`, `ErrorSuggestion`, `FlyDocUrl` | `internal/cli/cli.go:153-187` |
| pnpm | `[ERR_PNPM_CODE] message` / blank / hint body | `hint` on the log object, or a per-code reporter | `pnpm/src/reportError.ts` |
| Vercel | `Error: msg` / `Learn More: <link>` | `slug` or `link` on the error | `src/util/output/error.ts` |
| wrangler | `✘ [ERROR] title` plus indented notes (esbuild formatter, always colored) | Lines after the first become notes | `packages/wrangler/src/logger.ts:242-264` |
| gh | The message as-is, no prefix; usage after flag errors | Ad hoc: `Try authenticating with:  gh auth login` | `internal/ghcmd/cmd.go:187-300` |
| kamal | `  ERROR (Class): msg` in red, on **stdout** | none | `bin/kamal:8-17` |

uv is the best model because it separates the three things ployz mixes today:
- **Our sentence:** `error:`.
- **The raw upstream text:** `cause:`. This is where `Docker responded with status code 409 …` belongs.
- **What to do next:** `hint:`. This replaces `next:`, `Retry:` and `valid:`.

Its wrap width reserves the prefix and hangs continuation lines (`line_wrap.rs`). Errors print even under `-q` (`stderr_important`).

## Per tool

### uv (Rust)

- **Built on:** `anstream` + `owo-colors` (color), `indicatif` (progress), `console` (prompt input), `anyhow` + `thiserror`, `terminal_size` + `textwrap`. No table crate. Prompts and the error renderer are hand-written (`Cargo.toml` l.102-337).
- **(a) Lists:** hand-built padded columns with a dash rule (`commands/pip/list.rs:371-397`); trees with `(*)` for repeats (`uv-lock/src/lock/tree.rs`).
- **(b) Progress:** the `Printer` modes are `Silent | Quiet | Default | Verbose | NoProgress`. `--no-progress` (or `UV_NO_PROGRESS`) and `-v` hide the bars, and hidden bars fall back to plain lines. Summary lines are dimmed with a bold count. Change markers ` + ` / ` - ` / ` ~ ` are green, red and yellow.
- **(c) Errors:** see above. `warning:` reuses the same renderer in yellow (`uv-warnings/src/lib.rs`).
- **(d) Prompts:** `? msg [y/n] › yes` reads single keys and then becomes `✔ msg · yes` (`uv-console/src/lib.rs`). Without a TTY each caller skips the prompt or bails, e.g. `No username provided; did you mean to provide `--username` or `--token`?` (`auth/login.rs:70`).
- **(e) Palette:** error red bold, warning yellow bold, hint cyan bold, in-progress verbs cyan bold, completed verbs green bold, names bold, versions and timings dim.
- **(f) Machine output:** `--format json` / `--output-format json`, depending on the command. JSON goes to `stdout_important`, and stderr still carries progress (`tests/sync/sync.rs:1750-1795`). Preview schemas carry `"schema": {"version": "preview"}`.
- **(g) Color:** `--color` > `--no-color` > `NO_COLOR` > `FORCE_COLOR`/`CLICOLOR_FORCE` > anstream auto (TTY and `TERM != dumb`) (`settings.rs:174-199`). `CI` never changes output.

```
Resolved 7 packages in [TIME]
Prepared 7 packages in [TIME]
Installed 7 packages in [TIME]
 + blinker==1.7.0
```
```
error: Failed to add dependencies
  cause: No solution found when resolving dependencies
  cause: Because anyio was not found in the package registry and your project d…

hint: An index URL (http://[LOCALHOST]/basic-auth/simple) could not be querie…
```
(The first is from `tests/pip_install/pip_install.rs:1050`. The second is from `tests/it/auth.rs:68-74`, cut at the right edge by the grep tool; the snapshot strips the 2-space indent the renderer emits at `uv-errors/src/lib.rs:381`.)

### cargo (Rust)

- **Built on:** `anstream` + `anstyle`, `annotate-snippets` (diagnostics), `anstyle-progress` (OSC 9;4 taskbar progress), `anyhow`, `supports-hyperlinks`, `supports-unicode`. No indicatif and no prompt crate.
- **(a) Lists:** `cargo tree` in UTF-8 or ASCII; `cargo search` as padded `name = "ver"    # desc`. Data goes to stdout and the `note:` to stderr.
- **(b) Progress:** right-aligned 12-column status verbs in bright green bold (`shell.rs:484-503`), plus a transient one-line bar `    Building [=====>     ] 35/222: foo, bar`.
- **(c) Errors:** see above. Internal errors add `note:` lines asking for a bug report.
- **(d) Prompts:** effectively none. `cargo login` reads the token from stdin when stdin is not a terminal.
- **(f) Machine output:** `--message-format=json`, `cargo metadata` (one line, ignores a closed pipe, `shell.print_json`).
- **(g) Color:** `--color` / `term.color`, then anstream auto. `TERM=dumb` and CI disable the bar (`shell.rs:367-379`).

```
   Compiling foo v0.5.0 ([ROOT]/foo)
    Finished `dev` profile [unoptimized + debuginfo] target(s) in [ELAPSED]s
```
```
error: failed to parse manifest at `[ROOT]/foo/Cargo.toml`

Caused by:
  manifest is missing either a `[package]` or a `[workspace]`
```
```
error: no such command: `C`

help: a command with a similar name exists: `c`

help: view all installed commands with `cargo --list`
```
(These come from `crates/cargo-test-support/src/compare.rs:292-320`, `tests/testsuite/build.rs:434` and `cargo_command.rs:188`.)

### jj (Rust)

- **Built on:** `crossterm` (color, cursor, size), `sapling-streampager` (built-in pager), `pest` (template grammar), `thiserror`, `textwrap`. Progress, prompts, the error renderer and the formatter are hand-written.
- **(a) Lists:** everything goes through the template language and the graph renderer (`cli/src/config/templates.toml`, `graphlog.rs`).
- **(b) Progress:** described in the progress section above.
- **(c) Errors:** typed kinds map to headings and exit codes: User `Error:` 1, Config `Config error:` 1, Cli (clap) 2, BrokenPipe silent 3, Internal 255.
- **(d) Prompts:** `can_prompt()` is true when stderr is a TTY or `JJ_INTERACTIVE=1`. A prompt that has a default echoes `prompt: default` and takes it; one without a default errors.
- **(e) Palette:** labels in `cli/src/config/colors.toml` (`"error heading" = { fg = "red", bold = true }`). Labels nest. `--color=debug` prints `<<label::text>>`.
- **(f) Machine output:** no `--json`. Use `-T 'json(self) ++ "\n"'`.
- **(g) Color:** `NO_COLOR` sets `ui.color=never` as a config layer, so `--color` still wins. Auto checks the stdout TTY. The pager engages only when stdout is a TTY.

```
Error: Name `foo` is conflicted
Hint: Use commit ID to select single revision from: 96948328bc42, 401ea16fc3fe
Hint: Use `bookmarks(foo)` to select all revisions
```
```
Config error: Invalid type or value for ui.color
Caused by: wanted string or table

For help, see https://docs.jj-vcs.dev/latest/config/ or use `jj help -k config`.
```
(From `cli/tests/test_new_command.rs:899-902` and `test_global_opts.rs:717-720`.)

### Railway CLI (Rust)

- **Built on:** `colored`, `indicatif`, `inquire`, `console`, `anyhow` + `thiserror`, `is-terminal`, `ratatui`, `termimad`. Tables are a hand-made double-line box (`src/table.rs`).
- **(b) Progress:** on a TTY, a braille spinner ends as `Indexed`, then a `[=> ]` bar, `Compressed`, `Uploaded`. Off a TTY it appends `Indexing...` / `Uploading...` on **stdout**, and it returns without streaming logs unless CI mode is on (`src/commands/up.rs:207-336`).
- **(c) Errors:** anyhow Debug, then a cyan `→ hint`. Errors carry typed `code()`s such as `NOT_AUTHENTICATED`.
- **(d) Prompts:** inquire with a cyan `?`. Without a TTY: `Cannot prompt for confirmation in non-interactive mode. Use --yes to skip confirmation.` (`src/commands/delete.rs:49-54`). `src/exec_context.rs` centralizes json, ci, the TTY checks and agent detection.
- **(e) Symbols:** `✓` (86 uses), `→` (114), `…`, `●`, `✗`. No emoji.
- **(f) Machine output:** the header of `src/util/reporter.rs` states the contract: *"stdout carries result data only — a single JSON object (or an NDJSON stream for streaming commands) on success, or a single JSON error object on failure… stderr carries human progress, structured warnings."*
- **(g) Color:** `CI` is true unless empty, `false` or `0`. There is no explicit `NO_COLOR` handling; it relies on `colored` (unverified).

```
Not signed in.
  → Run `railway login` to authenticate, then re-run.
```
(Built from `src/errors.rs:217` and the reporter.)

### Bun (Rust port)

- **Built on:** nothing external for output. In-house `pretty_fmt!` markup (`<red>…<r>`) that strips tags at compile time when color is off. Also in-house: a `Progress.rs` port of Zig's `std.Progress`, `fmt::Table` and raw-mode prompts.
- **(a) Tables:** box-drawing when color is on, ASCII `| - |` when it is off (`src/bun_core/fmt.rs`).
- **(b) Progress:** emoji phase labels (`🔍 Resolving`, `📦 Installing`) appear only when stderr color is on.
- **(c) Errors:** lowercase `error` in red with a dimmed colon. No cause chain.
- **(d) Prompts:** `bun init` forces `auto_yes` when stdin is not a TTY.
- **(f) Machine output:** `--json` only on `pm`, `audit` and `info`.
- **(g) Color:** `FORCE_COLOR` > `NO_COLOR` > per-stream isatty. `TERM=dumb` means no color.

```
bun install v1.x.y (abcdef12)

+ react@18.2.0

12 packages installed [1.23s]
```
(Assembled from literals in `install_with_manager.rs:1134-1367`; the spacing is unverified.)

### gh (Go)

- **Built on:** `mgutz/ansi`, `briandowns/spinner`, `AlecAivazis/survey/v2` with `huh` as an experimental and accessible prompter, `go-isatty`, `glamour`, `cli/go-gh/v2` (tableprinter, term, jq, template).
- **(a) Tables:** `internal/tableprinter/table_printer.go`.
- **(b) Progress:** spinner on stderr, as covered above.
- **(c) Errors:** no prefix; `SilentError`; `AuthError` exits 4; `NoResultsError` prints only on a TTY and exits 0.
- **(d) Prompts:** `CanPrompt()` = `!neverPrompt && stdinTTY && stdoutTTY` (`iostreams.go:276-282`). Each command returns a FlagError naming the flag.
- **(e) Palette:** green `✓`, yellow `!`, red `X`, theme-aware `Muted` (`pkg/iostreams/color.go:192-210`).
- **(f) Machine output:** a bare `--json` lists the available fields. Output is colorized only when color is enabled and indented only on a TTY (`pkg/cmdutil/json_flags.go:99-247`).
- **(g) Environment:** `NO_COLOR`, `CLICOLOR=0`, `CLICOLOR_FORCE`, `GH_FORCE_TTY`, `GH_SPINNER_DISABLED`, `GH_PROMPT_DISABLED` (`pkg/cmd/root/help_topic.go:77-131`; implemented in go-gh, unverified).
- **Streams:** inconsistent. `repo archive` prints `✓` to stdout; `pr merge` prints it to stderr.

```
ID   TITLE                  BRANCH         CREATED AT
#32  New feature            feature        about 3 hours ago
```
```
32	New feature	feature	DRAFT	2022-08-24T20:01:12Z
```
```
--yes required when not running interactively
```
(The first two are TTY vs piped, from `pkg/cmd/pr/list/list_test.go:88-121`. The third is from `pkg/cmd/repo/delete/delete.go:70`.)

### flyctl (Go)

- **Built on:** `mgutz/ansi`, `aurora`, `morikuni/aec` (cursor), `termenv`, `briandowns/spinner`, `survey/v2`, `olekukonko/tablewriter`.
- **(a) Tables:** borderless and the same whether piped or not (`internal/render/render.go:32-72`).
- **(b) Progress:** the statuslogger draws a `-------` divider, then `" ⠋ [01/10] Waiting for machine X to reach a good state"` rows. Failures sort first, and finished rows linger 2 s.
- **(c) Errors:** `Error: …` plus description, suggestion and doc URL. Under `GITHUB_ACTIONS` with `FLY_GHA_ERROR_ANNOTATION` it also prints `::error title=flyctl error::…` (`internal/cli/cli.go:196-212`).
- **(d) Prompts:** `prompt: non interactive`, wrapped as `--yes flag must be specified when not running interactively`.
- **(f) Machine output:** `--json` / `FLY_JSON`, checked by hand in 84 command files, which drifts.
- **(g) Color:** `NO_COLOR`, `CLICOLOR=0`, `CLICOLOR_FORCE`. `IsCI` checks `CI`, `GITHUB_ACTIONS`, `BUILD_NUMBER`, `RUN_ID`.
- **Streams:** mixed between stdout and stderr.
- **Bugs not to copy:** `interactivelogger.go:53` prints the writer value, and `AsyncIterateWithErr` always returns nil.

```
> [1/3] Waiting for machine 148e… to reach a good state
Failed: <first line of error>
```
(Non-interactive form, built from `noninteractive.go:34-45`; the machine id is illustrative.)

### docker compose v2 (Go)

- **Built on:** `morikuni/aec`, `buger/goterm`, `go-runewidth`. Tables and final error printing come from docker/cli (unverified). No spinner library.
- **(a) Tables:** `ps` uses the same docker/cli tabwriter table whether piped or not. `--format json` gives a stream of objects.
- **(b) Progress:** covered above. `--progress tty` with `--ansi never` is an error: `can't use --progress tty while ANSI support is disabled`.
- **(c) Errors:** under `--progress json`, errors become `{"error":true,"message":"…"}` (`compose.go:192-222`). Cancel exits 130.
- **(d) Prompts:** raw-mode ` [y/N]: ` on a TTY; otherwise it reads a line from stdin. `-y` means "Assume "yes" as answer to all prompts and run non-interactively".
- **(e) Palette:** `cmd/display/colors.go`. Done and timer blue, count yellow, warning bold yellow, success green, error bold red. The marks are `✔ ! ✘` and the spinner is `⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏`.
- **(g) Color:** `NO_COLOR` forces `--ansi never`. `--ansi auto` checks the stdout TTY. Progress checks stderr.

```
[+] pull 1/2
 ✔ Image docker.io/library/nginx-l... Pulled  2.0s
 ⠋ Image docker.io/library/postgre... Pulling 0.0s
```
```
 Container e2e-ps-busybox-1 Started
```
(The TTY frame is from `cmd/display/tty_test.go:367-369`. The plain line is from `pkg/e2e/ps_test.go:49`.)

### Charm stack: lipgloss, huh, bubbletea, gum (Go)

- **Built on:** lipgloss v2, `colorprofile`, `x/term`, `x/ansi`.
- **Color profile** (`colorprofile/env.go:70-112`):
  - `TERM=dumb` gives NoTTY.
  - `NO_COLOR` caps the profile at ASCII.
  - `CLICOLOR_FORCE` raises it to ANSI even when piped.
  - `LightDark` / `AdaptiveColor` pick colors by background.
- **huh:** `WithAccessible(true)` drops the TUI for plain line prompts such as `Enter a number between 1 and 3:` and `[y/N]`. `TERM=dumb` turns it on automatically (`form.go:129-132`).
- **gum spin:** off a TTY it shows only the title (`spin/spin.go:162-164`).
- **Lesson for ployz:** use it for theming and accessible prompts. Charm is a TUI toolkit, not an output contract.

### Vercel CLI (TypeScript)

- **Built on:** `chalk`, `ora`, `cli-table3` (borderless), `@inquirer/*`, `ci-info`, `@vercel/detect-agent`.
- **(a) Tables:** cyan bold headers. The table goes to stderr; piped, the bare URLs go to stdout.
- **(c) Output methods:** one `Output` singleton on **stderr**. `> ` log lines, `WARNING! `, `Error: `, `> Success! `.
- **(d) Prompts:** `--non-interactive` turns on automatically for agents without a stdin TTY. Instead of prompting, it prints a JSON envelope `{"status":"action_required","reason":"missing_scope","message":…,"choices":[…],"next":[{"command":"vercel link --team <slug>"}]}` (`util/input/select-org.ts:151-175`).
- **(e) Symbols:** `▲` marks production, with labels aligned to a 16-column gutter. Emoji appear only when stdout is a TTY.
- **(g) Color:** `NO_COLOR=1` is matched exactly, so `NO_COLOR=true` doesn't count. That is a bug not to copy.

```
Error: Project is not linked. Run `vercel link` first.
```
```
Uploading [=====---------------]
```
(The first is a test string. The second is from `test/unit/commands/deploy/index.test.ts:841-853`, non-TTY at 25% steps.)

### wrangler (TypeScript)

- **Built on:** `chalk`, `cli-table3` (bordered), `prompts`, `@clack/core`, `log-update`, `ci-info`, and esbuild's message formatter.
- **Output:** the ` ⛅️ wrangler 4.x` banner, tables, spinner and JSON all go to stdout, so the banner has to be suppressed under `--json`.
- **Errors:** `✘ [ERROR]` and `▲ [WARNING]` with `color: true` hard-coded, so ANSI codes appear even in pipes.
- **Non-interactive prompts:** `confirm` falls back with `🤖 Using fallback value in non-interactive context: yes`. A prompt without a default throws `This command cannot be run in a non-interactive context`.
- **Machine output:** each deploy appends a `{type:"deploy",version:1,…}` NDJSON record to `WRANGLER_OUTPUT_FILE_PATH`, a sound idea for CI integrations.

```
 ⛅️ wrangler x.x.x
──────────────────
Uploaded test-name (TIMINGS)
Deployed test-name triggers (TIMINGS)
  https://test-name.test-sub-domain.workers.dev
Current Version ID: Galaxy-Class
```
```
X [ERROR] Unknown argument: asdf
```
(From `__tests__/deploy/core.test.ts:165-176` and `__tests__/kv/help.test.ts:84`; the test helper swaps `✘` for `X`.)

### pnpm (TypeScript, with a Rust port)

- **Built on:** `chalk`, `ansi-diff`, `boxen`, `cli-truncate`, `rxjs`, `@inquirer/prompts`, `ci-info`. The Rust port uses `console`, `owo-colors` and `insta`.
- **Architecture:** every component logs structured events, and a reporter renders them.
  - The default reporter is a live diff redraw. Lines that overflow the screen are committed to scrollback.
  - `append-only` repeats the progress line at most once a second.
  - `ndjson` writes the raw events.
  - `pnpm-render` re-renders a saved NDJSON log.
- **Errors:** `[ERR_PNPM_CODE] message`, a blank line, then the hint.
- **Streams:** stdout by default; commands that return data move the reporter to stderr (`useStderr`).

```
Progress: resolved 10, reused 5, downloaded 0, added 3, done
```
```
[ERR_PNPM_LOCKFILE_BREAKING_CHANGE] Lockfile /home/src/pnpm-lock.yaml not compatible with current pnpm: ...

Run with the --force parameter to recreate the lockfile.
```
(From `reporterForClient/reportProgress.ts:166-185` and `test/reportingErrors.ts:61`.)

### kamal (Ruby)

- **Built on:** Thor (`say msg, :magenta`) and SSHKit's Pretty formatter. No table, spinner or prompt library.
- **Output:** append-only, everything on stdout, errors included. Phases are magenta headings, followed by per-host command lines.
- **Errors:** red `  ERROR (Class): msg` (`bin/kamal:8-17`).
- **Prompts:** `--confirmed`/`-y`, otherwise Thor `ask` limited to `y`/`N`, with no TTY check.
- **Machine output:** no JSON; optional file and OTel loggers.
- **Lesson:** the per-host, append-only log reads well in CI but gives no overview on a TTY and nothing for machines.

```
Ensure kamal-proxy is running...
  INFO [8e5f0c41] Running docker ... on 1.2.3.4
  INFO [8e5f0c41] Finished in 0.412 seconds with exit status 0 (successful).
  Finished all in 12.3 seconds
```
(The heading and `Finished all` come from `lib/kamal/cli/main.rb` and `cli/base.rb:78-85`. The `INFO` lines are SSHKit's format, unverified.)

## Tables and lists: the TTY/pipe split

| Tool | TTY | Piped |
|---|---|---|
| gh | Aligned columns, UPPERCASE header, relative times, width-truncated | TSV, no header, RFC3339, extra columns |
| uv, cargo | Padded columns or trees on stdout | Same (no color) |
| compose, flyctl | Aligned table | Same aligned table |
| Vercel | Borderless table on stderr | Bare URLs on stdout |
| wrangler, pnpm outdated | Bordered box | Same |
| bun | Box-drawing | ASCII borders when color is off |

ployz's README question 4, option C ("aligned columns on a TTY, TSV when piped"), is gh's design. gh also drops the header when piped, which keeps `cut` and `awk` simple but hides column meaning. `--json` is the stable interface either way.
