# Uncloud (`uc`) CLI output: research

Source: `github.com/psviderski/uncloud` cloned with `--depth 1` to `/tmp/uncloud-src` at commit `c58108aa` (2026-10-02). All `file:line` citations point into that clone. Library citations into Docker Compose point at the Go module cache copy `~/go/pkg/mod/github.com/docker/compose/v2@v2.40.0/pkg/progress/` (written `compose/progress/` below).

How the examples were produced:

- **Real run**: `uc` built from the clone (`go build ./cmd/uc`) and run offline with an empty HOME.
- **Harness**: a 120-line Go program at `/tmp/uncloud-src/cmd/planrender/main.go` that builds `compose.Plan` values and progress events by hand, then calls Uncloud's own `Format()`, `tui.NewTable()`, `tui.PrintWarning` and `progress.RunWithTitle`. The text is Uncloud's real rendering code; only the data is invented. TTY frames were captured under `script(1)` at 100 columns.
- **Docs**: copied from `website/docs/2-getting-started/2-deploy-demo-app.md`, which shows output from a real cluster.
- **Reconstructed**: assembled from format strings because it needs a live cluster.

## 1. Summary

Uncloud's output feels good for five reasons. Destructive and multi-machine changes show a **plan before they run**: a styled tree with a one-line summary footer. Progress is **one redrawing block per operation**, labelled `<Kind> <name> on <machine>`, ending in a frozen final frame with ✔/✘ and per-line timings. Tables are **borderless, aligned and sparse**: a bold header, three spaces of padding, `-` for unknown values, and columns that show up only when they mean something. Color is **semantic and small**: green add, yellow change, red remove, faint for connective words, one accent color for names. Prompts and spinners **go to stderr and degrade to plain lines** when there is no terminal.

It is not a contract-driven system. Three of about 40 commands accept `-o json`. Partial results print a WARNING and exit 0. Errors are cobra's `Error: <wrapped chain>`. Plain-mode progress and the deploy plan carry raw ANSI escapes when piped, and `NO_COLOR` does not remove them.

## 2. Foundations

### 2.1 Libraries (`go.mod`)

| Library | go.mod | Used for |
|---|---|---|
| `charm.land/lipgloss/v2` v2.0.1 | `go.mod:9` | All styling (`Style.Render`), the borderless table (`lipgloss/v2/table`), the tree (`lipgloss/v2/tree`), `lipgloss.Println`, which downsamples color to the stdout profile |
| `charm.land/huh/v2` v2.0.1 | `go.mod:8` | Confirm prompts (accessible, line-based mode), Select pickers, the `huh/spinner` spinner |
| `charm.land/bubbletea/v2` v2.0.10 + `bubbles/v2` | `go.mod:6-7` | The custom "Connecting to …" spinner model |
| `github.com/charmbracelet/colorprofile` | `go.mod:18` | One explicit color-capability check (`cmd/uc/image/ls.go:220`) |
| `github.com/docker/compose/v2` v2.40.0, `pkg/progress` | `go.mod:27` | **The whole deploy/run/scale/rm/push progress renderer**: the `[+] Title n/m` block, spinners, ✔/✘, timers, the plain/json/quiet modes |
| `github.com/docker/cli` `cli/streams` | `go.mod:26` | `streams.Out`, which reports `IsTerminal()` for the progress writer |
| `github.com/morikuni/aec`, `github.com/buger/goterm` (indirect, via compose) | `go.mod:201`, `go.mod:89` | Cursor movement and terminal width inside compose's TTY writer |
| `golang.org/x/term` | `go.mod:55` | `IsTerminal` / `GetSize` in `tui` |
| `github.com/spf13/cobra` | `go.mod:46` | Commands, help, command groups, and the `Error: …` printing via `cobra.CheckErr` |
| `github.com/docker/go-units` | — | `HumanDuration` ("2 minutes ago") and `HumanSize` |

No tablewriter, no survey (survey only comes in indirectly through compose, `go.mod:65`). The stack is **Charm v2 for static rendering and prompts, plus Docker Compose's progress package, borrowed whole, for live progress.**

### 2.2 Internal output packages

| Path | What it owns |
|---|---|
| `internal/cli/tui/style.go:8-23` | The whole palette: `Faint`, `Red/Green/Yellow`, `Bold*`, `NameStyle` (bold, ANSI-256 color 152), `URLStyle` (underlined bright blue) |
| `internal/cli/tui/style.go:27-39` | `FormatImage`: the image name with a *faint* `:` before the tag |
| `internal/cli/tui/table.go:9-24` | `NewTable()`: no borders, bold header, `PaddingRight(3)` on every cell |
| `internal/cli/tui/prompt.go:13-36` | `Confirm(title)`: huh Confirm, `Yes!`/`No`, bold-yellow title, `WithAccessible(true)`, output to **stderr** |
| `internal/cli/tui/prompt.go:47-54` | `ThemeConfirmDanger`: bold-red title for destructive prompts |
| `internal/cli/tui/prompt.go:57-62` | `IsTerminalAvailable() = stdin is a TTY && stderr is a TTY` (stdout may be piped) |
| `internal/cli/tui/spinner.go:15-39` | `RunSpinner`: MiniDot spinner on stderr; without a TTY it prints the title once as a plain line |
| `internal/cli/tui/print.go:8-11` | `PrintWarning`: bold yellow `WARNING: …` on stderr |
| `internal/cli/progress/event.go:19-67` | Event-ID builders: `Faint("Container ") + name + Faint(" on ") + machine`, and the same for Image, Volume, Machine and the pre-deploy hook. **These IDs are the progress row labels.** |
| `internal/cli/cli.go:629-631` | `ProgressOut()` = `streams.NewOut(os.Stdout)`: compose progress goes to **stdout** |
| `internal/cli/connect.go:35-45,169-219` | Connect spinner: shown only after **500 ms**, on stderr, and erased on success. Plain stderr lines without a TTY. |
| `internal/cli/errors.go:4-15` | `CancelledError`, the only typed CLI error (a declined prompt) |
| `internal/cli/logs/formatter.go` | Log line layout and the per-machine/per-service color palette |
| `pkg/client/compose/plan.go:28-115`, `pkg/client/deploy/deploy.go:51-266`, `pkg/client/deploy/operation/*.go` `Format()` | Plan rendering lives **on the domain types**: every operation has `Format()` (styled, for humans) and `String()` (debug) |

Stream routing:

```
                         TTY (stdin+stderr)            not a TTY
plan, tables, results    stdout                        stdout (tables stripped of color;
                                                        plan and progress IDs keep ANSI)
compose progress         stdout, redrawing block       stdout, one line per event
connect / ps spinner     stderr, delayed, erased       stderr, one plain line
prompts                  stderr (huh)                  refused with "use --yes" (deploy,
                                                        scale) or read stdin, EOF = No (rm)
WARNING:                 stderr, bold yellow           stderr, still bold yellow
errors                   stderr "Error: …", exit 1     same
```

## 3. Output patterns

### 3.1 Lists and tables

All lists use `tui.NewTable()` and print through `lipgloss.Println`, which strips color when stdout is piped.

`uc ls` / `uc service ls` (`cmd/uc/service/ls.go:53-86`). Harness, piped:

```
NAME    MODE         REPLICAS   IMAGE                  ENDPOINTS
caddy   global       3          caddy:2.11
web     replicated   2          ghcr.io/acme/web:1.5   https://web.acme.com → :8000
```

- The ID column appears **only when two Services share a name** (`service/ls.go:55-60`). The same idea is used for HOOK in `ps` (`ps.go:126-139`). Columns exist only when they can tell rows apart.
- Lists inside a cell are joined with a faint `, ` (`service/ls.go:67-68`, `machine/ls.go:113`).
- Each row ends in trailing spaces (PaddingRight(3) applies to the last column too).

`uc machine ls` (`cmd/uc/machine/ls.go:62-122`): `NAME STATE ADDRESS PUBLIC IP WIREGUARD ENDPOINTS OS KERNEL ARCH DOCKER VERSION`. State is capitalised (`Up`), and missing values are `-` (`ls.go:71-106`).

`uc ps` (`cmd/uc/ps.go:123-187`): `SERVICE CONTAINER ID IMAGE CREATED STATUS IP ADDRESS MACHINE`. CREATED is relative (`2 minutes ago`). **Only STATUS is colored**: red for unhealthy, dead or OOM; green for healthy; yellow for other non-running states; plain for running (`ps.go:149-159`, `ps.go:228-242`). `--sort service|machine|health`. The data arrives behind `tui.RunSpinner("Collecting container info...")` (`ps.go:85`).

`uc inspect SERVICE` (docs):

```
Service ID: 4d2de1600b6ada221a03896cd388836c
Name:       excalidraw
Mode:       replicated

CONTAINER ID   IMAGE                          CREATED              STATUS                        IP ADDRESS   MACHINE
fde7ac7f11ad   excalidraw/excalidraw:latest   About a minute ago   Up About a minute (healthy)   10.210.0.3   machine-dc3c
```

That is aligned `Key:` lines for the object, a blank line, then a table for its children.

`uc ctx ls` marks the current row with `✓` in a `CURRENT` column (`cmd/uc/context/ls.go:45-50`).

Empty lists are a sentence: `No volumes found.` (`volume/ls.go`), `No images matching '%s' found.` (`image/ls.go:164-170`), `No contexts found` (no period, `context/ls.go:35`). `uc volume ls -q` prints bare names for scripting.

### 3.2 Deploy: plan, confirm, progress

Flow of `uc deploy` (`cmd/uc/deploy.go:87-266`):

```
load compose ─► build? ─► connect (spinner) ─► push images (progress)
   ─► plan ─► empty? ── yes ──► "Services are up to date."  exit 0
                │ no
                ▼
        "Deployment plan" + [context: X] + plan tree + summary
                ▼
        --yes / UNCLOUD_AUTO_CONFIRM? ─ no ─► TTY? ─ no ─► error "use --yes …"
                │ yes                          │ yes
                │                              ▼
                │                 "Proceed with deployment to X? [y/N]"
                │                              │ N ─► "Deploy cancelled. No changes were made." exit 1
                ▼                              ▼
        [+] Deploying to X  (redrawing progress block)
                ▼
        failure? ─► "Last 10 log lines from failed container:" + logs, then Error
```

**Plan rendering.** Harness, color removed. Every line comes from `Plan.Format` (`pkg/client/compose/plan.go:28-54`), `ServicePlan.Format` (`pkg/client/deploy/deploy.go:51-172`) and the operation `Format()` methods (`operation/container.go:70-76,100-108,141-149,261-276`, `operation/volume.go:43-49`):

```
Deployment plan

context: prod

+ create volume pgdata on alpha

+ create service db
  │   image: postgres:17
  │
  ╰── +   run container db on alpha

~ update service web
  │ ~ image:    ghcr.io/acme/web:1.4 → 1.5
  │   replicas: 2
  │
  ├── +/- replace container web/fde7ac7f11ad on alpha
  ╰── +/- replace container web/0a1b2c3d4e5f on beta

~ update service caddy (global)
  │ ~ image: caddy:2.10 → 2.11
  │
  ├── -/+ replace container caddy/998877665544 on alpha (stop-first)
  ╰── -   remove container caddy/aabbccddeeff on gamma

──────────────────────────────────────────────────────────────────────────────────────────
2 create · 2 replace (start-first) · 1 replace (stop-first) · 1 remove · across 3 machines
```

How it is built:

- The heading is bold and underlined. `context: X` is shown **only when more than one context exists** or `--connect` is used (`deploy.go:196-208`). The same target name goes into the prompt, so you can't deploy to the wrong cluster by accident (`deploy.go:219-226`).
- Modifiers carry the meaning: `+` bold green create, `~` bold yellow update, `-` bold red remove, `+/-` green start-first replace, `-/+` yellow stop-first replace, `▶` pre-deploy hook (`predeploy.go:197`).
- Within a line, verbs and connectives (`run container`, `on`, `image:`) are **faint**, and the nouns (Service, container, Machine) are normal or `NameStyle`. You read the line by its nouns.
- Image diff: when only the tag changes, the repo is printed once and the tag goes red → green (`repo:`**`1.4`**` → `**`1.5`**). Digests or different repos print in full, old → new (`deploy.go:230-266`).
- Tree glyphs `│ ├── ╰──` are faint (`deploy.go:144-169`).
- Footer: a faint `─` rule exactly as wide as the summary (`plan.go:48`). The summary gives colored counts per operation kind, joined by a faint ` · `, and ends `across N machine(s)` (`plan.go:57-115`).
- Only image and replicas are diffed; there is a TODO for the rest (`deploy.go:94`).

**Confirmation** (docs; the prompt is huh's accessible mode, `prompt.go:27-28`, rendering `Title [y/N] `):

```
Proceed with deployment to default? [y/N] y
```

**Progress.** `progress.RunWithTitle(ctx, fn, uncli.ProgressOut(), title)` (`deploy.go:241-246`). Client code emits events through `progress.ContextWriter(ctx).Event(...)` with IDs from `internal/cli/progress/event.go`. Compose's writer picks the mode from `out.IsTerminal()` (`compose/progress/writer.go:138-170`).

TTY mode (`compose/progress/tty.go`) redraws the block every 100 ms with cursor-up and hides the cursor. Each line is `spinner ID [child progress] Text Status … elapsed`. Children are indented under their parent; an image pull is a child of its container. The title line counts done/total (`tty.go:170`). The final frame stays on screen. Harness, last frame, color removed:

```
[+] Deploying to prod 4/4
 ✔ Container web-x7k2 on alpha                 Healthy                                         0.9s
   ✔ Image ghcr.io/acme/web:1.5 on alpha         Pulled                                        0.3s
 ✔ Container web/fde7ac7f11ad on alpha  Removed                                                0.2s
 ✘ Container web-p9q1 on beta                  Unhealthy                                       0.2s
Error: deploy services: container 'web/3fa2b1c4d5e6' failed to become healthy: container exited with code 1
```

Docs show the same shape from a real cluster:

```
[+] Deploying to default 2/2
 ✔ Container excalidraw-0z12 on machine-dc3c          Healthy              30.6s
 ✔ Container excalidraw/fde7ac7f11ad on machine-dc3c  Removed               0.4s
```

Symbols: braille spinner `⠋⠙⠹…` in yellow while working, `✔` green when done, `!` yellow on a warning, `✘` bold red on an error (`compose/progress/event.go:194-208`, `colors.go`). Timers are blue. The title turns blue when everything is done (`tty.go:170-173`). Byte-level child progress uses a braille bar `⠀⡀⣀⣄⣤⣦⣶⣷⣿` (`tty.go:348`).

Not a TTY (`compose/progress/plain.go:42-48`): one line per event, `fmt.Fprintln(out, prefix, e.ID, e.Text, e.StatusText)`. There is no title, no ✔, and no timing; each line starts with a space (the empty dry-run prefix); and **the faint ANSI codes inside the IDs remain**. Harness, `| cat -v`:

```
 ^[[2mImage ^[[mghcr.io/acme/web:1.5^[[2m on ^[[malpha  Pulling
 ^[[2mContainer ^[[mweb-x7k2^[[2m on ^[[malpha  Creating
 ^[[2mContainer ^[[mweb-x7k2^[[2m on ^[[malpha  Healthy
 ^[[2mContainer ^[[mweb^[[2m/^[[mfde7ac7f11ad^[[2m on ^[[malpha  Removed
```

**Failure context.** When a container fails its health check or a pre-deploy hook fails, deploy prints `Last 10 log lines from failed container:` in bold red, then the container's last log lines through the normal log formatter, or `<no logs available>`, before the error (`deploy.go:247-297`). `UNCLOUD_FAILED_CONTAINER_LOGS_TAIL` changes the count (`deploy.go:302-313`). If fetching the logs fails, it prints the command to run by hand: `You can try manually with: uc logs web/3fa2b1c4d5e6`.

**Nothing to do.** `Services are up to date.` and exit 0 (`deploy.go:188-191`). `uc scale` prints `Service web is already scaled to 2 replicas.` (`scale.go:111`).

`uc scale` repeats the same plan, confirm and progress flow under a `Scaling plan` heading, with the progress title `Scaling service web (1 → 3 replicas)` (`cmd/uc/service/scale.go:115-197`). That flow is copied by hand into `deploy.go`, `scale.go` and `caddy/deploy.go`; it is not a shared component.

### 3.3 Prompts and confirmations

- **Confirm**: `tui.Confirm(title)`. It is line-based (accessible mode), writes to stderr, takes `[y/N]` with default No, and has a bold-yellow title (`prompt.go:13-45`). A danger variant with a bold-red title is used when `machine init` would reset an existing member. It prints red bullet consequences first (`internal/cli/machine.go:101-133`):

  ```
  The remote machine is already initialised as a cluster member. Resetting it will:
  - Remove all service containers from the machine
  - Reset the Uncloud daemon on the machine to the uninitialised state
  Do you want to reset it first? [y/N]
  ```

- **Show the blast radius, then ask.** `machine rm` prints `Found 2 service containers on machine 'beta':` and a lipgloss tree (`• web (replicated, 2 containers)` with children `name • image • state`), then a sentence saying what will happen, then `Do you want to continue? [y/N]` (`cmd/uc/machine/rm.go:101-140,192-230`). `volume rm` lists ` • 'pgdata' on machine 'alpha'` before asking (`cmd/uc/volume/rm.go:84-96`).
- **Declined**: a `CancelledError` with one sentence that ends in what did *not* happen: `Deploy cancelled. No changes were made.`, `Cancelled. Machine was not removed.`, `Caddy deploy cancelled. The machine has been added to the cluster.` (`deploy.go:233`, `machine/rm.go:139`, `machine/add.go:250`). It prints on stderr without the `Error:` prefix and exits 1 (`cmd/uc/main.go:163-167`).
- **Without a TTY**: deploy and scale refuse with `cannot ask to confirm deployment plan in non-interactive mode, use --yes flag or set UNCLOUD_AUTO_CONFIRM=true to auto-confirm` (`deploy.go:214-217`). `machine rm` and `volume rm` skip that check and read stdin; EOF counts as No.
- **Pickers**: a huh `Select` for `uc ctx use` with no argument (`cmd/uc/context/use.go:54-79`), `ctx connection`, and the machine choice in `volume create`. Without a TTY: `cannot select a context interactively without a terminal. Pass the context name explicitly: uc ctx use CONTEXT`.
- `--yes/-y` binds to `UNCLOUD_AUTO_CONFIRM` (`deploy.go:44,76-78`).

### 3.4 Errors

There is one path: handlers return wrapped errors (`fmt.Errorf("connect to cluster: %w", err)`), and `cobra.CheckErr` prints `Error: ` plus the whole chain on stderr and exits 1 (`main.go:168`). Real runs:

```
Error: connect to cluster: no cluster contexts found in the Uncloud config (/tmp/uchome/.config/uncloud/config.yaml). Please initialise a cluster with 'uncloud machine init' first
Error: load compose file(s): open /nonexistent.yaml: no such file or directory
Error: invalid value for --sort: "x", must be one of 'service', 'machine' or 'health'
Error: unsupported output format 'yaml' (supported: json)
Error: unknown command "bogus" for "uc"

Did you mean this?
	logs
```

Every one of these exits 1, usage mistakes included. The next step is inline prose inside the error, as in `Please initialise a cluster with 'uncloud machine init' first` (which names the wrong binary). The longest is the `machine rm` proxy error, which carries a docs URL (`machine/rm.go:86-92`). `SilenceUsage: true` keeps usage text out of runtime errors (`main.go:43-44`).

### 3.5 Success lines

Short sentences, with names in single quotes or `NameStyle`, and a period most of the time:

- `Current cluster context is now 'default'.` (`context/use.go:78`)
- `Volume 'pgdata' removed from machine 'alpha'.` (`volume/rm.go:110`)
- `Machine reset initiated and will complete in the background.` (`machine/rm.go:160`)
- After `uc run`, a block with the Service's endpoints (`service/run.go:166-172`):

  ```
  excalidraw endpoints:
   • https://excalidraw.sh8hsb.uncld.dev → :80
  ```

Many mutating commands print **nothing after the frozen progress block**: the ✔ lines are the success message.

### 3.6 Warnings and partial results

`tui.PrintWarning` prints bold yellow `WARNING: <lowercase message>` on stderr (`print.go:8-11`). When one machine fails in a fan-out (`ps`, `image ls`, `volume ls`, `machine rtt`, `ListServices`), Uncloud warns and carries on: `WARNING: failed to list service containers on machine gamma: <grpc error>` (`ps.go:210-213`). **The exit code stays 0**, and nothing marks the table as incomplete. Library code in `pkg/client/service.go:105,130,338` and `pkg/client/volume.go:75` prints these warnings straight to stderr. The exception is `caddy cert ls`, which prints what it has and then fails with `certificate list is partial: …`, exit 1 (`cmd/uc/caddy/cert/ls.go:72-80`).

### 3.7 Logs

Layout: `<faint StampMilli time> <bold machine, padded> <bold service>/<faint 5-char container id> [hook] <message>`. Columns are padded to the longest known name. Colors come from a 10-color palette, applied to the machine name when one Service is shown and to the service name when several are (`internal/cli/logs/formatter.go:57-110,175-190`). A container's stderr lines go to stderr (`formatter.go:140-145`). Stream problems become `WARNING: log stream from container 'web/3fa2b1c4d5e6' on machine 'beta' stopped responding` (`formatter.go:148-174`).

### 3.8 Colors and symbols

| Meaning | Style | Where |
|---|---|---|
| Add / healthy / done | green, bold green for the marker | plan `+`, ps STATUS, ✔ |
| Change / caution | yellow, bold yellow | plan `~`, `-/+`, stop-first, WARNING, prompt title, spinner |
| Remove / failure | red, bold red | plan `-`, ✘, danger prompt, failed-logs header |
| Connective words, separators, tree lines, attribute keys | faint | `on`, `run container`, `image:`, `│ ├──`, `·`, `,`, the `:` in images |
| Names you act on | `NameStyle` = bold + ANSI-256 color 152 (pale cyan) | Service, context, volume in plans and prompts |
| URLs | underlined bright blue | help footer, version |
| Section heading | bold + underline | `Deployment plan`, `Scaling plan` |
| Timers | blue | progress |

Glyphs: `✔ ✘ !` (progress), `✓` (current context), `•` (bullets), `→` (diffs, endpoints), `·` (summary separator), `│ ├── ╰──` (plan tree), braille spinner and bar.

### 3.9 Modes, non-TTY and machine output

- **Interactive**: chosen per stream. Prompts and spinners need stdin+stderr to be a TTY (`prompt.go:57-62`); compose progress checks whether stdout is a TTY (`writer.go:155`).
- **Plain**: you get it by piping. There is no `--no-color`, no `--progress=plain` flag, and no `NO_COLOR` support in the plan or progress paths. Compose has `ModeQuiet` and `ModeJSON` (`writer.go:122-134`), but Uncloud never sets `progress.Mode`.
- **JSON**: only `uc machine ls -o json`, `uc caddy cert ls -o json` and `uc version -o json` (the last also takes a Go template). The JSON is the pretty-printed internal struct (`json.MarshalIndent(machines.ToNative())`, `machine/ls.go:52-58`), with Go field names in PascalCase (`"Version"`, `"GitCommit"`). Errors stay `Error: …` text even with `-o json`. Nothing else has machine output; `volume ls -q` is the only other scripting aid.

## 4. Weaknesses: do not copy

1. **ANSI leaks into pipes.** `fmt.Println(style.Render(…))` emits escapes whatever the target. Real run: `uc --help | cat -v` prints the docs URL with an escape sequence around every character (`^[[4;94;4mh^[[m^[[4;94;4mt^[[m…`, `main.go:125-128`). `NO_COLOR=1` changes nothing. Only the tables, which go through `lipgloss.Println`, are clean. The fix is structural: make the writer strip escapes, not each call site.
2. **Styled IDs break both the plain output and the alignment.** Progress IDs carry faint escapes (`progress/event.go:23-25`). Plain mode prints them raw, and compose measures padding with `len()` on the escaped string (`tty.go:179`), so the status column wobbles (see `Removed` against `Healthy` in both frames above). Keep labels as data; style them at render time.
3. **The deploy plan and progress go to stdout, but spinners and prompts go to stderr.** `uc deploy > out.txt` captures a progress log mixed with the plan, and nothing machine-readable.
4. **Partial fan-outs exit 0** after a stderr WARNING, and library code prints its own warnings (`pkg/client/service.go:130`), so callers cannot collect them.
5. **No exit-code taxonomy.** Unknown command, bad flag, not found, cancelled and failed deploy all exit 1.
6. **Errors are the wrapped chain** (`connect to cluster: failed to connect …: all connections (2) … failed; last error: …`). Hints are buried in prose and sometimes wrong (`uncloud machine init`).
7. **JSON is an afterthought.** It covers three commands, serializes internal structs with PascalCase keys, has no error object, no envelope, and no `--json` on mutations.
8. **Copy-pasted flows.** The plan, context line, confirm and progress steps are repeated in `deploy.go`, `scale.go` and `caddy/deploy.go`. Summary counting exists twice (`plan.go:57-115` and `deploy.go:175-226`). There are no golden tests for any `Format()`.
9. **Inconsistent small things**: `No contexts found` without a period, `'name'` quoting in some messages and `NameStyle` in others, trailing spaces on every table row, a TTY check in `deploy` but none in `machine rm`, and the remote install script's raw output (Docker's banner, `⏳`, `✓`, `🎉`) streamed unfiltered into `machine init`.
10. **The plain mode loses information**: no title, no done/total count, no final ✔ summary, no timings.

## 5. What Ployz needs that Uncloud lacks

- **A `--json` contract on every command**: one object on stdout, errors as `{"error":{code,message,details}}`, warnings folded into the result, progress on stderr or NDJSON. Uncloud has none of this.
- **Partial results as a first-class outcome**: a `failures` / `omitted` list per Machine, an exit code (Ployz's 3), and a human rendering that marks the table as incomplete. Uncloud only warns.
- **Three explicit modes** (interactive TTY, plain non-interactive, JSON), chosen once at startup from flags, env and the TTY state, with every renderer obeying that choice. Uncloud decides per call site and per stream.
- **Plan as data.** `deploy --plan` and `--plan --json` should share one plan value with the human tree. Uncloud's plan exists only as a pre-confirm display: there is no dry-run flag, and the `Format()` strings can't be emitted as JSON.
- **Follow-up hints as data** (`next`, `valid`) that the human renderer prints in one format. Uncloud writes hints as prose inside error strings.

## 6. What Ployz should copy

1. **One plan renderer with Uncloud's grammar**: a `+`/`~`/`-` (`+/-`, `-/+`) modifier, a verb, the object, `on <server>`; a faint `│ ├── ╰──` tree of per-Server operations under each Service; `old → new` diffs that print a shared prefix once (`repo:1.4 → 1.5`). Build it as one type with `render_human()` and `serde::Serialize`, so `ployz deploy --plan`, `--plan --json`, `env sync --plan` and `diff` share it, and use only `→` (Ployz mixes `->` and `→` today).
2. **The summary footer**: a faint rule exactly as wide as the summary, then colored counts per kind joined by ` · `, ending `across N servers`. Use the same footer for plans and for the end of progress (it replaces `1 ready · 1 created · 1 machine`).
3. **A progress block with one row per (operation, Server)**. Labels are `<Kind> <name> on <server>` with the kind and `on` faint. Children are indented (image pull under container). The title shows a `done/total` count, rows show ✔/✘/! and elapsed time, and the last frame stays on screen. Keep the label as plain data and style it at draw time (Uncloud weakness 2). In plain mode print the same rows as complete lines (`✔ Container web on alpha  Healthy  0.9s`) at state changes, not as a raw event log. In Rust: `indicatif::MultiProgress` or a small crossterm redraw (Ployz already depends on crossterm).
4. **Show the plan, then ask, and name the target in the prompt**: `Proceed with deployment to prod? [y/N]` on stderr, defaulting to No, and only on a TTY. Without a TTY, refuse with the exact flag to pass. A declined prompt prints one sentence that says what did *not* happen (`Deploy cancelled. No changes were made.`). This fits Ployz's `--confirm` refusal: prompt on a TTY, keep the refusal plus a `next:` line elsewhere.
5. **Show the blast radius before a destructive confirm**: a short tree of what will be lost (`• web (replicated, 2 containers)` → `name • image • state`), then one sentence saying what the command will do. This suits `server rm`, `project rm` and volume loss, where Ployz prints the loss as one long sentence today.
6. **Attach failure context to failed deploys**: on an unhealthy container or a failed hook, print `Last 10 log lines from <service> on <server>:` and the tail through the normal log formatter, with an env/flag override. If the fetch fails, print the exact `ployz logs …` line to run. This turns Ployz's raw `create Container failed: Docker … 500` passthrough into something you can act on.
7. **Borderless aligned tables on a TTY**: a bold header, 3-space gutters, `-` for unknown values, relative times (`2 minutes ago`), color on the one status column only, and optional columns (ID only when names collide, HOOK only when there are hooks). This matches Ployz's "names first, ids only when needed". Trim trailing spaces, and keep TSV when piped if scripts depend on it.
8. **A tiny semantic palette in one module**: green add/ok, yellow change/caution, red remove/fail, faint for connective words and separators, one accent style for names you act on, underline for URLs. Every renderer imports these names and never raw colors. Route all output through a writer that strips styling when the target isn't a color terminal or `NO_COLOR` is set (in Rust, `anstream` does this per stream), so Uncloud's leak can't happen.
9. **Spinner etiquette**: show a spinner only after about 500 ms, draw it on stderr, erase it on success, and fall back to one plain stderr line without a TTY (`internal/cli/connect.go:35-45,169-219`). Use it for `ps`, `logs` connect and `server add` waits instead of `Waiting for …` lines.
10. **A short "nothing to do" result** in place of an empty plan or no-op success: `Services are up to date.`, `Service web is already scaled to 2 replicas.` Exit 0. This also fixes Ployz bugs 2, 3 and 10, which claim changes on a no-op.
