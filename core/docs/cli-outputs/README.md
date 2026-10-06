# ployz CLI outputs

This folder captures what `ployz` prints today, command by command, across four setups. This page catalogs the output patterns in those captures so a uniform output design can start from facts.

## Regenerate

Regenerating takes two steps. A driver writes raw captures to a directory outside the repo, and `publish.sh` copies a finished run over the tracked files.

```
<group>/run.sh [PARENT]                  publish.sh DIR
  DIR = fresh mktemp -d under PARENT       refuses unless DIR/.ok exists
  (default /tmp), outside any checkout     refuses symlinks, non-files and
  writes raw *.md and *.ansi                 names not tracked in <group>/
  writes DIR/.ok last, naming <group>      scrubs a copy with scrub.sh
  prints DIR                               replaces each tracked file by
                                             temp file + mv; deletes nothing
```

A driver that stops early, on a failed setup step or Ctrl-C, exits non-zero without writing `.ok`, so `publish.sh` refuses its directory and the tracked captures stay as they were. Its raw captures stay in DIR for inspection. They are unscrubbed and can hold token secrets. If `publish.sh` itself fails partway, for example on a full disk, some tracked files are already replaced; free the space and rerun it on the same DIR.

These scripts are dev tooling that an operator runs on their own checkout. They protect against accidents: failed steps, partial runs, interrupts, full disks and wrong run directories. They don't protect against an attacker who plants files in `/tmp` or the repo.

- [store/run.sh](store/run.sh): needs only the debug binary `core/target/debug/ployz`; it runs against the hidden SQLite Store (`PLOYZ_STORE=sqlite:…`) under an isolated HOME.
- [cloud/run.sh](cloud/run.sh): starts its own seeded local Cloud with `dashboard/scripts/verify/up.sh` and stops only that one at the end (`KEEP=1` leaves it running). It refuses to run while this checkout already has a verify Cloud (`dashboard/.verify/run`), because `up.sh` would wipe it, including the one `verify-cluster up --cloud` uses. Billing-on is not captured.
- [cluster/run.sh](cluster/run.sh): needs privileged Docker, `cargo build -p ployz -p ployzd`, and `docker pull ghcr.io/getployz/ployz-testkit:main`; it builds alpha, beta, gamma plus a lone `solo`, then tears them down. Its Cluster, image and HOME carry a random per-run id, so runs from two checkouts never collide. It stops when a setup capture that later captures depend on fails, such as `server add`.
- [cloud-cluster/run.sh](cloud-cluster/run.sh): needs a paired run from `scripts/verify-cluster up --machines 1 --daemon stable --cloud`; call it as `MANIFEST=…/cluster.json SECTIONS="deploy runtime domain env errors up" run.sh [PARENT]`. Without `MANIFEST` it takes the newest run, and without `SECTIONS` it runs all six. `up` appends to `deploy.md`, so it needs `deploy` earlier in the same run. It refuses a run whose manifest lacks a `.cloud` block or whose `state` is not `ready` (a torn-down run keeps its manifest as `removed`), and one that does not answer.

- [record.py](record.py): runs one command in a PTY and writes an asciicast, a GIF (`agg`) and a last-frame PNG (`ffmpeg`), as in `record.py --gif out.gif --png out.png -- 'ployz deploy'`. It writes nothing unless the command exits with `--expect-exit` (default 0), and replaces each output through a temp file and a rename. [prototype-media/make.sh](prototype-media/make.sh) uses it to re-record the prototype GIFs.
- [cap.sh](cap.sh): appends one command's capture to a `.md` and its terminal rendering to the matching `.ansi`. Every driver captures through it.
- [capture-dir.sh](capture-dir.sh): creates a driver's fresh output directory under PARENT, so two drivers never share one, and refuses a PARENT inside a git checkout.
- [publish.sh](publish.sh): copies a finished driver run over the tracked captures, as drawn above.
- [scrub.sh](scrub.sh): the scrub `publish.sh` runs. Public IPv4 addresses become `203.0.113.10`, because Servers report the capturing host's public IP, and secrets (`ployz_` tokens, `pmet_` enrollment tokens, `ppair_` pairing secrets) keep their prefix and become `<prefix>_<redacted>`.

Each `.md` holds one block per command with its exit code, stdout and stderr. Each `.ansi` holds the same commands run under a terminal. Exit 124 means the capture's own timeout killed the command.

Totals: 692 command blocks (store 240, cloud 165, cluster 105, cloud-cluster 182). Exits: 0 x380, 1 x243, 2 x37, 3 x16, 124 x8, 127 x1, 7 x1, none x6 (piped or typed TTY runs).

## The contract as the code states it

From `core/crates/ployz/src/output.rs` and `failure.rs`:

```
                  without --json             with --json
result            stdout (human text)        stdout: one JSON object
progress, tables  stdout                     stderr
WARNING: lines    stderr                     stderr, and copied into "warnings"
error             stderr + hint lines        stdout: {"error":{code,message,details}}
streaming         stdout lines               stdout: one JSON object per line
prompts           only on a TTY (stdin+out)  never
```

- `finish` prints the JSON value or runs the human closure. `show` prints pretty JSON in both modes (inspect-style commands).
- `warn` prints `WARNING: …` on stderr and adds the text to the next JSON result's `warnings`.
- Fan-outs print `{…, failures, omitted}` and exit 3 if either is non-empty.
- After a result is printed, any later error goes to stderr and the exit becomes 3.
- Human errors print the message, then `valid: a, b` from `details.valid_children`, then `next: X` from `details.next` unless the message already holds that command.
- Exit codes: 0 success; 1 any command failure; 2 a rejected command line (`USAGE_EXIT`, "as clap exits"); 3 partial (`PARTIAL_EXIT`, "The result is printed, but some targets failed or never answered"). `ployz exec` passes through the remote exit code.

## 1. Lists and tables

Dominant: tab-separated rows with an UPPERCASE header. 24 header lines across the captures (SERVICE 7, CONTAINER ID 5, ID 4, ORGANIZATION 2, KIND 2, VOLUME 2, INSTALLATION 1, NAME 1).

- Header TSV. `SERVICE	PRIVATE DNS	SOURCE	NEXT DEPLOY` ([store/service.md](store/service.md)). The current row is marked in one of three ways: a `*` column in ctx ls ([store/context.md](store/context.md)), a ` *` suffix in org ls (`ada *	Ada Lovelace's Projects`, [cloud/org.md](cloud/org.md)), or no mark at all.
- Wide header TSV. server ls has 14 columns, ID first: `ID	NAME	MEMBERSHIP	STORAGE	SUBNET	GATEWAY	PUBLIC IP	…	ARCH` ([cluster/server.md](cluster/server.md)).
- Headerless TSV. project ls: `blog	production*` ([store/project.md](store/project.md)). domain ls: `acme.com	web → PORT	Setting up · Deploying` ([cloud/domain.md](cloud/domain.md)).
- Space-separated, no header. deployment ls: `#1 queued Saved revision 1 939f4f20-…`, then `next: ployz deployment ls --cursor 38 --limit 5` for paging ([cloud/deploy.md](cloud/deploy.md)).
- Indented prose list. env ls: `Environments of Project shop:` then `  production (default)` ([store/env.md](store/env.md)).
- `key = value` lines. get: `web.replicas = 2`, with `- (default)` for unset values ([store/settings.md](store/settings.md)).
- Log lines. `2026-10-05T01:41:07.849606203+00:00 gamma web/24674546193d | tick` ([cluster/ps-logs-exec.md](cluster/ps-logs-exec.md)). Server logs drop the fractional seconds. `-n 3` is per container, so `logs web -n 3` printed 6 lines. Lines follow the container's stream, so nginx notices land on stderr ([cloud-cluster/runtime.md](cloud-cluster/runtime.md)).

Empty lists vary:

- A sentence with a hint: `No Projects yet. Create one: ployz project new NAME` ([store/project.md](store/project.md)).
- A sentence alone: `No Services in shop/production.` and `No contexts found` (lowercase body, no period) ([store/context.md](store/context.md)).
- Header plus a sentence: `INSTALLATION	ACCOUNT	REPOSITORIES` then `No repositories yet. Next: ployz github connect` ([cloud/github.md](cloud/github.md)).
- The header alone: service ls on an empty Environment ([store/service.md](store/service.md)).

## 2. Success and confirmation lines

Dominant: one past-tense sentence naming the object and its scope. Of 297 sentence-like success lines, 157 end with a period and 140 do not.

- Staged. `Staged web.replicas in shop/production (revision 10).` 38 lines take this shape ([store/settings.md](store/settings.md)). Multi-object forms print `Staged: db, web, volumes.pgdata` on a second line (6 times, [store/env.md](store/env.md)).
- Made or removed. `Created Project shop with Environment production.` ([store/project.md](store/project.md)), `Revoked token a261eda1-8592-433b-b55f-21aaad1f75cf.` ([cloud/token.md](cloud/token.md)).
- No period. `Updated Server gamma (0523a6681d894d128f69ea28b7ed62d5)` ([cluster/server.md](cluster/server.md)), `Switched context to 'default'` (single quotes, [cluster/server.md](cluster/server.md)).
- Two phrasings for the same removal. project rm: `Took an Environment off the Servers (Deployment #4).` env rm: `Removed shop/hotfix from the Servers (Deployment #3).` ([cloud-cluster/deploy.md](cloud-cluster/deploy.md), [cloud-cluster/env.md](cloud-cluster/env.md)).
- Multi-line with a secret. `Made token ci (…) in Organization ada, expiring 2027-01-03T01:12:26.123Z.` then `Its secret is shown only now; set it as PLOYZ_TOKEN:` then the secret ([cloud/token.md](cloud/token.md)).
- Status block. `Organization ada (ada@example.com).` / `Environment shop/production.` / `2 staged changes.` / `Deployment 1 is queued.` / `next: ployz diff` ([cloud/auth.md](cloud/auth.md)).
- Raw TSV. service stop, start and restart print `stop	shop-production/api	3ba1dcbd…	01479b917a80…` with full 64-hex container ids ([cluster/service.md](cluster/service.md)).
- Arrow line. `127.0.0.1:36547 -> 10.210.2.3:80 (shop-production/web/…); Ctrl-C stops` ([cluster/service.md](cluster/service.md)).

Change arrows differ by command: diff and `deploy --plan` print `->` ([store/staging.md](store/staging.md)); `env sync --plan` and domain ls print `→` ([store/env.md](store/env.md)); explain prints `web.replicas — Replicas` ([store/settings.md](store/settings.md)).

## 3. Progress and following

Two renderers exist.

Plain status lines (deploy, up, retry, deployment start, env rm, project rm). 26 `Following Deployment` lines:

```
Following Deployment #36; stopping this leaves it running.
queued: web pending, api pending, postgres pending, worker pending, pg-data pending
running: web pending, api pending, postgres pending, worker pending, pg-data pending
applied: web unchanged, api unchanged, postgres unchanged, worker unchanged, pg-data unchanged
Deployment #36 of shop/production: applied
  web: unchanged
```

([cloud-cluster/deploy.md](cloud-cluster/deploy.md)). The output is the same on a TTY. Under `--json` the progress lines move to stderr. `--events FILE` writes NDJSON like `{"nodes":[…],"status":"queued","type":"deployment"}` ([cluster/deploy.md](cluster/deploy.md)). The local Store prints no `Following` line. `up` adds `  uploaded by Ada Lovelace, base 5a3ec95`, `Open https://workspace.ada.ployz.test` and `Dashboard: http://localhost:…` ([cloud-cluster/deploy.md](cloud-cluster/deploy.md)).

A redrawing renderer (server add, for the ingress). It is the only output with ANSI escapes: cursor-up, clear, bold, cyan and green ([cluster/server.ansi](cluster/server.ansi)). Its last frame:

```
✔ Container ployz-create-… on alpha  Healthy
✓ Deployed to default
  1 ready · 1 created · 1 machine
```

Other waits print one line and block: `Waiting for approval... Signed in to … as ada@example.com in Organization ada.` ([cloud/auth.md](cloud/auth.md)) and `Waiting for ssh://root@172.19.0.3 to participate: …; retrying for up to 299s.` ([cluster/server.md](cluster/server.md)).

## 4. Prompts and confirmations

Dominant: no prompt. A destructive command without `--confirm` refuses (exit 1, `confirmation_required`), names the loss and prints the retry:

```
Removing Project scratch1 deletes every Environment in it (production) …; this can't be undone. No changes made.
Retry: ployz project rm scratch1 --confirm scratch1
```

([store/project.md](store/project.md)). A TTY gets the same refusal with no prompt.

- Wrong value. `--confirm scratch does not match Project scratch1. No changes made.` exits 2, not 1 ([store/project.md](store/project.md)). Same for org rm and env rm.
- What you type differs: `--confirm scratch1` for a Project, `--confirm shop/qa` for an Environment ([store/env.md](store/env.md)).
- Volume loss. `This permanently deletes the data of store. Accept each by name.` then `Retry: ployz deploy --accept-volume-loss store --expect-version 16:7:0.10:3d1ac37da6e0cfb1` ([cluster/volume.md](cluster/volume.md)).
- `--yes`. cloud reset refuses with `…then confirm with --yes` and `next: ployz cloud reset --yes` ([cloud/deploy.md](cloud/deploy.md)).
- Numbered picker on a TTY. `Select a context:` / `  1. dev` / `  2. prod (current)` / `> 1`. A bad choice prints `invalid selection` ([store/context.md](store/context.md)). Without a TTY: `cannot Select a context interactively without a terminal; pass the context name: ployz ctx use <context-name>`.
- Inline prompt. `Storage preparation [zfs/none] (none is Docker only, not recommended): ` with the next WARNING printed on the same line ([cluster/server.md](cluster/server.md)).

## 5. Warnings

Dominant: `WARNING: <sentence>.` on stderr. 38 lines, 9 distinct texts.

| Count | Text (start) | Capture |
|---|---|---|
| 23 | `WARNING: Live Observation is observer-relative and not globally complete` | [cluster/ps-logs-exec.md](cluster/ps-logs-exec.md) |
| 5 | `WARNING: Docker only (not recommended): Volumes on this Server get no size limits, …` | [cluster/server.md](cluster/server.md) |
| 3 | `WARNING: Docker volume (not recommended): no size limit, …` | [cluster/volume.md](cluster/volume.md) |
| 2 | `WARNING: Server gamma is running Services: … Move them off first: ployz server drain gamma` | [cluster/errors.md](cluster/errors.md) |
| 2 | `WARNING: Machine … was omitted` | [cluster/errors.md](cluster/errors.md) |
| 2 | `WARNING: the Service selection came from a partial Live Observation` | [cluster/errors.md](cluster/errors.md) |
| 1 | `WARNING: Machine … failed: target Machine RPC timed out` | [cluster/errors.md](cluster/errors.md) |
| 1 | `WARNING: Replicated Services may now be under-replicated: shop-production/web. …` | [cluster/server.md](cluster/server.md) |
| 1 | `WARNING: No Server here can host Managed volumes yet, …` | [cluster/volume.md](cluster/volume.md) |

The Live Observation warning also fires on a healthy `ps` that exits 0. One warning starts lowercase. Under `--json` the same text also appears in a `warnings` array.

## 6. Errors

Human errors go to stderr. 193 human failure messages and 89 JSON error objects were captured.

First-line casing: 117 start uppercase, 57 start lowercase, 19 are clap's `error:`.

- Dominant, uppercase without a period: `No Service named nope in Environment production` then `valid: web, db` ([store/service.md](store/service.md)).
- Lowercase: `not signed in to Cloud` ([cloud/errors.md](cloud/errors.md)), `no Organization nope of yours` ([cloud/org.md](cloud/org.md)), `context nope not found in Ployz config …` ([store/context.md](store/context.md)), `target Machine RPC timed out` ([cluster/errors.md](cluster/errors.md)), `at least one setting flag is required` ([cluster/server.md](cluster/server.md)). 60 lowercase occurrences across 21 distinct messages.
- Clap: `error: the following required arguments were not provided:` plus a Usage line ([cloud/errors.md](cloud/errors.md)). Under `--json` the whole clap text, Usage included, goes into `message` with code `invalid_argument`.
- With a sentence-style next step: `Cloud couldn't reserve the Cluster Domain; deploy again.` ([cloud/deploy.md](cloud/deploy.md)).

Not-found wording varies for the same idea:

- `No Service named nope in Environment production` ([store/service.md](store/service.md))
- `No running Service "nope"; ployz ps lists what's running` ([cluster/ps-logs-exec.md](cluster/ps-logs-exec.md))
- `No Server named "nope"; ployz server ls lists them` ([cluster/errors.md](cluster/errors.md))
- `Server nope was not found` from server drain ([cluster/errors.md](cluster/errors.md))

Raw upstream errors pass through:

- `Cloud answered HTTP 409: {"_tag":"PublicError","code":"CONFLICT",…}` ([cloud/deploy.md](cloud/deploy.md))
- `Cloud answered HTTP 405: <!doctype html>…` as `invalid_argument` ([cloud/errors.md](cloud/errors.md))
- `could not reach Cloud at http://127.0.0.1:9: error sending request: client error (Connect): tcp connect error: Connection refused (os error 111)` ([cloud/errors.md](cloud/errors.md))
- `Machine RPC failed: Docker operation failed: Docker responded with status code 409: container … is not running` ([cloud-cluster/errors.md](cloud-cluster/errors.md))
- `create Container failed: Docker operation failed: Docker responded with status code 500: error from registry: denied` ([cloud-cluster/deploy.md](cloud-cluster/deploy.md))
- `inspect Machine upgrade worker: No such file or directory (os error 2)` ([cluster/server.md](cluster/server.md))
- `all 1 connections from the explicit connection failed: connection attempt failed: SSH connection to root@10.255.255.1 timed out after 3 seconds; …` ([cluster/server.md](cluster/server.md))

## 7. Follow-up hints

Dominant: a trailing `next: <command>` line.

| Form | Count | Example | Capture |
|---|---|---|---|
| `next:` line | 82 in 19 files | `next: ployz login` | [cloud/errors.md](cloud/errors.md) |
| `valid:` line | 24 | `valid: web, api, postgres, worker` | [cloud/domain.md](cloud/domain.md) |
| `Retry:` line | 6 | `Retry: ployz project rm blog --confirm blog` | [store/project.md](store/project.md) |
| `Undo it:` line | 2 | `Undo it: ployz env sync --undo 8fe5f75f-… --project shop` | [store/env.md](store/env.md) |
| inline `X lists …` | 8 | `; ployz ps lists what's running` | [cluster/ps-logs-exec.md](cluster/ps-logs-exec.md) |
| inline `Next:` | 1 | `No repositories yet. Next: ployz github connect` | [cloud/github.md](cloud/github.md) |
| inline `Create one:` | 1 | `No Projects yet. Create one: ployz project new NAME` | [store/project.md](store/project.md) |

- `next:` targets vary in scope: `ployz diff --project shop`, `ployz diff`, `ployz deploy --expect-version 21:0:0.0 --project shop`.
- Human output drops many hints the JSON carries. 48 of 120 JSON success objects have a top-level `next`. Only 25 of 250 human successes print `next:`. service add, set, domain add and env branch carry `next` in JSON only ([store/service.md](store/service.md)).
- `valid:` can be very long. The never-sync error lists about 40 rows; the unknown Setting error lists 24 names ([store/settings.md](store/settings.md)).

## 8. Identifiers

Dominant: names (Project, Environment, Service, Server, Volume, Organization). Machine-shaped ids still show up in many places:

- UUIDs. Token ids, and token rm takes only the UUID: `token rm ci` fails with `no token or signed-in device ci in this Organization` ([cloud/token.md](cloud/token.md)). Sync ids for `--undo` ([store/env.md](store/env.md)).
- Deployments have a number and a UUID. `Following Deployment #36`, `Deployment 1 is queued.` (no `#`, [cloud/auth.md](cloud/auth.md)), `Deployment 939f4f20-… built nothing.` ([cloud/deploy.md](cloud/deploy.md)). A bad id exits 2 with `Expected a Deployment ID or number` ([cluster/deploy.md](cluster/deploy.md)).
- 32-hex Machine ids. The ps MACHINE column (`01479b917a80	api	service	3ba1dcbd6b854ea699b76f6ef296576d	running`), server ls ID, `Updated Server gamma (0523a…)`, and `WARNING: Machine 0523a…` ([cluster/ps-logs-exec.md](cluster/ps-logs-exec.md)).
- Container ids. 12 hex in ps and logs, 64 hex in service restart and port-forward ([cluster/service.md](cluster/service.md)).
- One Service, two ids. JSON has a UUID `id` and a hex runtime `service_id` ([cloud-cluster/runtime.md](cloud-cluster/runtime.md)).
- Version strings. `21:0:0.0`, `17:1:2.1`, `16:7:0.10:3d1ac37da6e0cfb1`, `31:cf4a853b16980e99` ([store/staging.md](store/staging.md), [cloud/deploy.md](cloud/deploy.md), [cluster/volume.md](cluster/volume.md), [store/env.md](store/env.md)).
- Runtime names. `shop-production/web` in TSV and warnings, `shop/production` in prose.

## 9. Partial results and unreachable Machines

Captured with gamma stopped ([cluster/errors.md](cluster/errors.md)) unless noted.

| Command | Exit | What it says |
|---|---|---|
| ps | 3 | `WARNING: Machine 0523… failed: target Machine RPC timed out` |
| ps --json | 3 | `WARNING: Machine … was omitted` plus `omitted` |
| server ls | 3 | No warning; gamma row says `up` with STORAGE `Volume support unknown` |
| service restart | 3 | `WARNING: the Service selection came from a partial Live Observation`, printed twice |
| logs, server logs | 0 | Nothing; gamma's lines are just missing |
| server inspect gamma | 1 | `target Machine RPC timed out` |

A failed Deployment also exits 3, with the result on stdout and nothing on stderr: `Deployment #1 of shop/production: failed` / `  No Server is enrolled in this Organization` ([store/errors.md](store/errors.md)).

## 10. Human output that is really JSON

- server inspect prints the same pretty JSON with or without `--json` ([cluster/server.md](cluster/server.md)).
- schema prints a 3132-line JSON Schema ([store/help.md](store/help.md) points to it).
- JSON inside human lines: `web.healthcheck = {"path":"/healthz","timeoutSeconds":300}` and `web.env.API_KEY = {"secret":true}` ([store/settings.md](store/settings.md)), `web.source: {…} → {…}` ([store/env.md](store/env.md)), `env={"GREETING":"hello"}` in service inspect ([cluster/service.md](cluster/service.md)).

## 11. `--json` shape

Dominant: one pretty-printed object on stdout (120 of 124 success outputs). The other 4 are NDJSON streams (logs, server logs, events).

- Errors are compact, on one line: `{"error":{"code":"not_found","details":{…},"message":"…"}}`. Codes seen: not_found 23, invalid_argument 21, unauthenticated 16, conflict 12, unavailable 9, unsupported 3, confirmation_required 3, internal 1, ambiguous 1.
- Mutations share `environment`, `staged`, `next`, `immediate`, `typed_addresses` ([store/settings.md](store/settings.md)).
- Fan-outs add `failures` and `omitted` ([cluster/errors.md](cluster/errors.md)). server add adds `warnings` ([cluster/server.md](cluster/server.md)).
- Time fields come in three forms: epoch seconds (`admitted_at`), ISO strings (`expires_at`), and `created_at_unix_nanos` ([cluster/ps-logs-exec.md](cluster/ps-logs-exec.md)).
- Keys are snake_case except `canRestore` ([store/staging.md](store/staging.md)).
- Secrets: ps --json shows env values as `<redacted>`. `deploy --plan --json` dumps full container specs under `preview` ([cluster/deploy.md](cluster/deploy.md)).
- 18 `--json` runs also printed non-warning human text on stderr: followers, `login --wait`, server add, `env shutdown`'s `applied: ` and others. stdout stayed one object.

## 12. Exit codes

| Exit | Count | Meaning seen |
|---|---|---|
| 0 | 380 | success |
| 1 | 243 | any failure: not found, conflict, unauthenticated, missing `--confirm`, `does not support --json` |
| 2 | 37 | clap errors, `--confirm` mismatch, `Expected a Deployment ID or number`, local `deploy --detach`, bare `ployz org` |
| 3 | 16 | partial fan-out, failed Deployment, error after a result was printed |
| 7, 127 | 2 | `ployz exec` passing through the remote code ([cloud-cluster/runtime.md](cloud-cluster/runtime.md)) |

Exit 2 is applied by hand in about ten handlers through `.with_exit(USAGE_EXIT)`, so similar usage mistakes split between 1 and 2. `ployz exec` can return 1, 2 or 3 from the remote command, which overlap ployz's own codes ([cluster/ps-logs-exec.md](cluster/ps-logs-exec.md)).

## 13. Help text

- Bare `ployz` prints help on stdout and exits 0. Bare `ployz org` and `ployz github` print help on stderr and exit 2 ([cloud/org.md](cloud/org.md), [cloud/github.md](cloud/github.md)).
- Summaries mix casing: `server      Manage Servers` but `service     Manage services`; the tagline is `Manage Ployz machines, services, and volumes` ([store/help.md](store/help.md)).
- Commands are mostly alphabetical, but `link`, `github`, `token` and `project` are out of place.
- Every subcommand repeats the globals. `--connect <connect>` has no description. `--ployz-config` shows the env path.
- Placeholders mix casing: `<name>`, `<context>`, `<SECONDS>`, `<PROJECT>`, `<VERSION>`, `<FILE>`.
- project rm help leaves `<name>` blank and says `--yes cannot bypass this` on a command that has no `--yes`.
- Top-level help ends with a `Settings catalog: …` line. `--version` prints `0.2.3`.

## Likely bugs

Each item was checked against the captures.

1. `unset web.env.MISSING` says `No change in shop/production: already set.` and exits 0, while `get` of the same name fails. [store/settings.md](store/settings.md)
2. `publish` on an Environment with nothing staged claims `Published empty/production as Saved revision 1.` [store/staging.md](store/staging.md)
3. `discard` with nothing staged claims `Discarded every staged change in empty/production (revision 1).` [store/staging.md](store/staging.md)
4. After `org rm` of the acting Organization, `org ls` fails with `…is no longer one of yours` and `next: ployz org ls`, which loops. [cloud/org.md](cloud/org.md)
5. A Managed volume whose deploy failed can't switch: `Volume cache storage cannot change after deployment has been requested`, after an unrelated Docker WARNING. [cluster/volume.md](cluster/volume.md)
6. Local `deploy --detach` exits 2 with `…so it can't detach`, yet `deployment ls` then shows `#6 queued`. [cluster/deploy.md](cluster/deploy.md)
7. server ls with gamma down exits 3 with no warning and shows gamma as `up`. [cluster/errors.md](cluster/errors.md)
8. A wrong `--confirm` exits 2; a missing `--confirm` exits 1. [store/project.md](store/project.md)
9. server upgrade prints its failure line twice on stdout, then the cause again on stderr. [cluster/server.md](cluster/server.md)
10. `domain add` of an existing domain prints `Staged … (revision N)` although nothing changed. [cloud-cluster/domain.md](cloud-cluster/domain.md)
11. `env shutdown qa` on a never-deployed Environment prints a bare `applied: `, then `This Deployment already ended`. [store/env.md](store/env.md)
12. `cannot Select a context interactively…` capitalizes Select mid-sentence. [store/context.md](store/context.md)
13. Signed-out `ps` and `logs` report `could not inspect /run/ployz/ployz.sock: Permission denied (os error 13)` instead of a sign-in hint. [cloud/errors.md](cloud/errors.md)
14. The server rm `Retry:` line includes `--ployz-config '~/.config/ployz/config.yaml'`; the quoted `~` won't expand. [cluster/errors.md](cluster/errors.md)
15. server rm removes first, then warns `Move them off first: ployz server drain gamma`, and prints `Volumes the Cluster loses (0)` with an empty list. [cluster/server.md](cluster/server.md)
16. service restart prints the partial Live Observation warning twice. [cluster/errors.md](cluster/errors.md)
17. logs and server logs exit 0 with no warning when a Machine is down. [cluster/errors.md](cluster/errors.md)
18. server drain `--json` prints `public_key` as a byte array; other commands print a base64 string. [cluster/server.md](cluster/server.md)
19. `server inspect nope --json` uses code `invalid_argument`, not `not_found`. [cloud-cluster/errors.md](cloud-cluster/errors.md)
20. A registry error's embedded newline (`denied` / `denied`) breaks the indented Deployment result. [cloud-cluster/deploy.md](cloud-cluster/deploy.md)
21. up prints `  build workspace from the upload: built ` with an empty detail. [cloud-cluster/deploy.md](cloud-cluster/deploy.md)
22. Clap Usage lines expose the hidden global: `Usage: ployz service add --image <REF> --project <project> --ployz-config <ployz-config> <name>`. [store/service.md](store/service.md)
23. `exec` of a missing binary prints `OCI runtime exec failed: …` on stdout, exit 127. [cloud-cluster/errors.md](cloud-cluster/errors.md)
24. status for a directory linked to a deleted Project says `Needs attention: No Project named workspace` with no repair step. [cloud-cluster/deploy.md](cloud-cluster/deploy.md)
25. The `--secret` empty-stdin error's JSON details carry an unrelated postgres `example`. [store/settings.md](store/settings.md)
26. `env branch --live` reads `db can't be used live: the Branch copies it: nothing running can lend it`, with two colons. [store/env.md](store/env.md)
27. `deploy --expect-version 1` is a clap parse error (exit 2); a stale but well-formed version exits 1. [cloud/deploy.md](cloud/deploy.md)
28. server add over SSH ends on `Waiting for … retrying for up to 299s` and exits 0 with no joined line. [cluster/server.md](cluster/server.md)
29. `deployment start 1` with no Server follows forever after `queued:`; the capture had to kill it. [cloud/deploy.md](cloud/deploy.md)
30. Human `service add`, `set`, `domain add` and `env branch` omit the `next` their JSON carries. [store/service.md](store/service.md)
31. The `Docker only` prompt prints its WARNING on the prompt's own line. [cluster/server.md](cluster/server.md)
32. Bare `ployz org` prints help on stderr with exit 2; bare `ployz` uses stdout and 0. [cloud/org.md](cloud/org.md)

## Open design questions

1. **Exit codes for usage mistakes.** Today 2 is set by hand in some handlers, so a wrong `--confirm` is 2 and a missing one is 1.
   A. Exit 2 only for clap parse errors, 1 for everything else.
   B. Exit 2 for every "your command line is wrong" error, set in one place by error code.
   C. Keep per-handler choices.

2. **One hint vocabulary.** Today there are `next:`, `Retry:`, `Undo it:`, `valid:`, inline `Next:`, `Create one:` and `ployz ps lists …`.
   A. Only `next:` for commands and `valid:` for choices; everything else becomes one of them.
   B. Keep a few named labels (`next:`, `retry:`, `undo:`, `valid:`), all lowercase, always on their own line.
   C. Keep today's mix.

3. **Should human output print every `next` the JSON carries?** 48 JSON successes carry `next`; 25 human ones print it.
   A. Yes, always.
   B. Only when the next step is not obvious (for example after a stage, say "deploy").
   C. Never on success; hints only on errors.

4. **List format.** Today: header TSV, headerless TSV, space-separated, indented prose and `key = value`.
   A. Every list is header TSV; empty lists print the header plus one `No … yet.` line.
   B. Header TSV for tables, `key = value` for single objects, nothing else.
   C. Aligned columns on a TTY, TSV when piped.

5. **Hex ids in human output.** Machine ids, 64-hex container ids and UUIDs appear in tables, warnings and success lines.
   A. Names only; ids live in `--json` and inspect.
   B. Names first, with a short id where two things can share a name.
   C. Keep ids where they appear today.

6. **Partial results.** Today ps warns and exits 3, server ls exits 3 silently, logs exits 0 silently.
   A. Every fan-out prints one `WARNING: Machine X did not answer` line and exits 3.
   B. Warn, but exit 0 unless nothing answered.
   C. Leave it per command.

7. **Progress rendering.** Deploys print plain status lines; server add redraws with ANSI.
   A. Plain lines everywhere.
   B. Redraw on a TTY, plain lines when piped, for both.
   C. Keep both as they are.

8. **Error text.** 117 errors start uppercase, 57 lowercase, and raw HTTP bodies, Docker errors and OS errors pass through.
   A. One sentence in our words, capitalized, no period; the raw cause only in JSON `details`.
   B. Our sentence, then `cause: …` on a second line with the raw text.
   C. Keep passing the raw text through, but fix casing.
