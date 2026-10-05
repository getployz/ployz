# One output system for the ployz CLI: proposal

Status: proposal, 2026-10-05. It builds on the catalog in [README.md](README.md) and the research in [research/](research/).

It is based on the decisions the operator made after reading the catalog:

- Change everything obvious now, because no one uses the CLI yet.
- Exit codes are decided in one place.
- Hint labels are uniform and lowercase.
- Success hints appear only when the next step isn't obvious.
- An error is our sentence plus a `cause:` line.
- Long ids are hidden unless names clash.
- Unanswered Servers always produce a warning and exit 3.
- Output has three modes. Uncloud is the visual reference.

## 1. Foundation

Every command goes through one module, `ployz::ui`. Handlers pass in values and typed parts. They never write strings that already contain layout or color. The API sketch is in [research/rust-libs.md](research/rust-libs.md#sketch-ployzui).

| Concern | Choice | New crates | Who else does it this way |
|---|---|---|---|
| Color and env detection | anstyle + anstream | 0 (clap already compiles them) | uv, cargo |
| Tables | hand-rolled, on the unicode-width code we already have | 0 | gh (aligned on a TTY, TSV when piped) |
| Live progress | indicatif | 4 | uv, cargo-like tools |
| Prompts | dialoguer | 1 | console-rs family |
| Errors | hand-rolled `error:` / `cause:` / hint lines | 0 | uv, jj |

anstream strips escape codes from any stream that isn't a color terminal. That fixes, in one place, the bug Uncloud has at every call site: colors leaking into pipes. It honors `NO_COLOR`, `CLICOLOR`, `CLICOLOR_FORCE` and `TERM=dumb`, and a new global `--color auto|always|never` flag overrides all of them.

## 2. Three modes

The mode is decided once, before dispatch, and every renderer obeys it.

```
--json given? ── yes ──► Json
     │ no
stderr is a TTY, TERM is not dumb, and CI is unset? ── yes ──► Interactive
     │ no
     ▼
   Plain
```

Streams are the same in every mode. This is the rule jj, uv, cargo and Vercel follow:

```
stdout   the result: a list, a record, a done-sentence, or the JSON object
stderr   everything else: progress, warnings, errors, prompts, notes
```

`ployz service ls > out.txt` therefore captures only the table, and `ployz deploy 2> /dev/null` shows only the result. Under `--json`, an error is still a JSON object on stdout (Railway does the same), so a script reads one object whatever happened.

| | Interactive | Plain | Json |
|---|---|---|---|
| color | yes | no | no |
| lists | aligned columns, bold header (when stdout is a TTY) | UPPERCASE-header TSV | `{noun: [...]}` |
| progress | see section 5 | one line per state change, plus a heartbeat | human lines on stderr; `--events FILE` writes NDJSON |
| prompts | yes | refuse, naming the flag | refuse, naming the flag |
| hints | `next:` lines | the same lines | keys in `details` (see section 3) |

## 3. Vocabulary

**Palette.** Colors are chosen by meaning, from one table, as jj does. Call sites never pick a color.

| Tone | Style | Used for |
|---|---|---|
| `Good` | green | ✔, created, healthy |
| `Change` | yellow | ~, updated, warning |
| `Bad` | red | ✘, removed, failed, `error:` |
| `Muted` | faint | connective words (`on`, `in`), separators, timings |
| `Name` | bold | names you act on |
| `Link` | underline | URLs |

**Marks.** The only marks are `✔ ✘ ! + ~ - →`, plus a spinner in Interactive mode. ASCII `->` disappears.

**Hint lines.** There are five labels. Hints are always lowercase, on their own line, and on stderr.

```
next: ployz deploy --project shop      a command to run now
inspect: ployz logs worker --machine beta   a read-only command that shows more
retry: ployz project rm blog --confirm blog
undo: ployz env sync --undo s3 --project shop
valid: did you mean restartPolicy?     the closest choice, before the list
valid: web, api, postgres, worker      choices, capped at 8 then "and 32 more"
```

Under `--json` each hint is a key in the error's `details`: `next`, `retry` and `undo` are one command string each, `inspect` is an array of commands, `did_you_mean` is one name, and `valid_children` is the full, uncapped list.

`next:` is reserved for the one obvious next step, so a command prints at most one. `inspect:` lines are extra, never instructions, and any number may print. Inline hints such as `; ployz ps lists what's running`, `Next:` and `Create one:` become one of these five lines. `; ployz ps lists what's running` becomes `inspect: ployz ps`.

**Names, not ids.** Human output shows names. An id appears only as a disambiguator when two things share a name, the way Uncloud adds its ID column only on a clash. Ids always stay in `--json`. Deployments use `#37` everywhere and drop the UUID. Container and Machine hex ids leave the tables.

## 4. Each output, in each mode

### Lists

```
Interactive (stdout)                        Plain (stdout, piped)
NAME       IMAGE              STATUS        NAME	IMAGE	STATUS
web        acme/web:1.5       ✔ running     web	acme/web:1.5	running
worker     acme/worker:1.5    ✘ crashed     worker	acme/worker:1.5	crashed
postgres   postgres:17        -             postgres	postgres:17	-
```

- Only the status column carries color.
- Unknown values print `-`.
- No trailing spaces.
- An empty list prints one sentence on stderr, `No Services in production yet.`, and the header on stdout when piped.

### One record

```
Interactive                Plain
name      web              name = web
image     acme/web:1.5     image = acme/web:1.5
replicas  2                replicas = 2
```

### Done

One sentence in our words. A hint follows only when the next step isn't obvious.

```
Staged web on production.
next: ployz deploy --project shop
```

A no-op says so: `Nothing staged; production already matches.` That covers the bugs where unset, publish and discard claimed work they didn't do.

### Warnings and partial results

```
! beta did not answer; its rows are missing.          (stderr, exit 3)
```

Every fan-out uses this one line and exit 3. Under `--json` it goes into `omitted`/`failures`, as it does today.

### Errors

```
error: Could not reach Cloud at https://api.ployz.dev.
  cause: Connection refused (os error 111)
next: ployz cloud status
```

- The first line is ours, capitalized, and names the domain thing.
- One `cause:` line follows: the deepest `Error::source()` in the chain, raw. The layers in between stay in JSON.
- This needs one refactor: our `thiserror` types must stop interpolating `{source}` into `#[error]`, or the cause prints twice.
- Raw HTTP bodies and Docker errors move into `cause:`, never the first line.

JSON is unchanged except that it gains `cause`: every source in the chain, outermost first. It is always a list and is empty when there is none.

`{"error":{"code":"unavailable","message":"…","cause":["…"],"details":{"next":"ployz cloud status"}}}`

### Prompts

Prompts appear in Interactive mode only. In both other modes the command refuses with exit 2 and names the flag, as gh and flyctl do:

```
error: Removing blog deletes 3 Services and the pg-data Volume.
retry: ployz project rm blog --confirm blog
```

The refusal is `confirmation_required`, and under `--json` the command to rerun is `details.retry`.

Before a destructive prompt, the command shows what will be lost as a short tree. Declining prints what did *not* happen: `Cancelled. Nothing was removed.` and exits 130.

## 5. Progress

Every long operation emits one stream of typed events: `(task, server, state, detail)`. This is the shape docker compose and pnpm both use. Renderers only read those events, so how Interactive mode looks is a decision that doesn't change the data model. Plain and Json render the same in both options below.

**Plain mode, always.** One line per state change, plus a heartbeat every 30 s so CI can see the command is alive:

```
Deploying #37 of shop/production to 2 servers
  web on alpha: starting
  api on alpha: unchanged
  web on alpha: healthy (2.1s)
  worker on beta: starting
  still waiting: worker on beta, starting for 30s
  worker on beta: unhealthy (30.0s)
error: Deployment #37 failed: worker on beta is unhealthy.
  cause: container exited with code 1
Last 10 log lines from worker on beta:
  panic: missing DATABASE_URL
inspect: ployz logs worker --machine beta
retry: ployz deploy --project shop
```

A failed Deployment always ends with the tail of the failed Container's log, an `inspect:` line with the exact `ployz logs` command for that Service on that Server, and `retry:`. If the tail can't be fetched, the `inspect:` line still prints. That replaces today's `queued: web pending, api pending, …` lines, which repeat every Service on every tick.

**Interactive mode: two options.**

```
A. Lines only (no redraw)              B. One live block, final frame stays
   Plain's lines, with color and          (Uncloud, compose, flyctl, uv, pnpm)
   ✔/✘ marks. Nothing moves.
                                          Deploying #37 to 2 servers      3/4
  ✔ web on alpha  healthy 2.1s             ✔ web on alpha      healthy    2.1s
  ✔ api on alpha  unchanged                ✔ api on alpha      unchanged
  ✘ worker on beta  unhealthy 30.0s        ✘ worker on beta    unhealthy 30.0s
                                           ⠋ postgres on alpha starting   4.2s
```

What the surveyed tools do in a terminal:

| Tool | Redraws a multi-row block | Single spinner line | Lines only |
|---|---|---|---|
| Uncloud, docker compose, flyctl, pnpm, uv | ✔ | | |
| cargo, jj, Bun, gh, Railway, Vercel, Wrangler | | ✔ | |
| Kamal | | | ✔ |

Every tool except Kamal moves something on screen, and every one of them falls back to plain lines when it isn't writing to a terminal. A costs nothing extra. B adds indicatif (4 crates: console, encode_unicode, indicatif, unit-prefix) and one renderer, and replaces the hand-rolled redraw in `deploy/apply.rs`, which doesn't clamp to the terminal height today.

Decided 2026-10-05: **B**, built after Plain. The deploy view is where Uncloud feels best, and with B it is a renderer over events we need anyway.

A runnable prototype is in [prototype/](prototype/) (`cargo run --release -- deploy [--fail] [--plain] [--json]`, `cargo run --release -- ls`). Recordings: [success](prototype-media/success.gif), [failure](prototype-media/fail.gif), [piped](prototype-media/plain.gif), [lists](prototype-media/ls.gif).

Both options end with the same summary footer, taken from Uncloud:

```
───────────────────────────────────────────────
2 updated · 1 unchanged · 1 failed · across 2 servers
```

## 6. Exit codes

The code is decided in one place, from the error code. No handler sets it.

| Exit | Meaning | Error codes |
|---|---|---|
| 0 | done, including a no-op | |
| 1 | it ran and failed | not_found, conflict, unauthenticated, unavailable, unsupported, internal, a failed Deployment |
| 2 | your command line is wrong | invalid_argument (including `--json` on a command without it), confirmation_required, ambiguous, every clap error |
| 3 | partial: a result printed, but some Servers failed or didn't answer | |
| 130 | cancelled: Ctrl-C or a declined prompt | |

`unsupported` means this Server, Cloud or connection can't do what was asked: an OS, architecture, protocol version or image store we don't support, or a Cloud without CLI access. The command line was fine, so it exits 1.

`ployz exec` still passes the remote exit code through. Its own failures before the remote command starts use the table above.

## 7. Rollout

These ship as five stacked PRs, each verified on the verify-server cluster with `cap.sh` captures before and after. The times are guesses for one agent.

1. **`ui` core.** Modes, palette, `--color`, the error renderer and centralized exit codes. This replaces `failure::terminate`. About 1 day.
2. **Lists, records, done-lines and hints.** Move about 236 `say!` sites and 20 `eprintln!` sites. Delete `output.rs`'s human paths. About 1 to 2 days.
3. **Progress.** The event model, the Plain renderer and the Interactive renderer (A or B). About 1 day, plus half a day for B.
4. **Prompts.** dialoguer, the loss tree, and `--confirm` refusals. About half a day.
5. **The 32 bugs in [README.md](README.md#likely-bugs) that this doesn't already fix.** This includes ids, partial results and wording. About 1 day.

Each PR adds snapshot tests of all three modes. In Interactive mode a test forces `--color always`.
