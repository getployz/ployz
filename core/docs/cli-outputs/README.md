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
- [cloud/run.sh](cloud/run.sh): starts its own seeded local Cloud with `dashboard/scripts/verify/up.sh` and stops only that one at the end (`KEEP=1` leaves it running). It refuses to run while this checkout already has a verify Cloud (`dashboard/.verify/run`), because `up.sh` would wipe it, including the one `verify-cluster up --cloud` uses. Its org rm of babbage is approved the same way, as Ada ([approve.sh](approve.sh)).
- [cluster/run.sh](cluster/run.sh): needs privileged Docker, `cargo build -p ployz -p ployzd`, and `docker pull ghcr.io/getployz/ployz-testkit:main`; it builds alpha, beta, gamma plus a lone `solo`, then tears them down. Its Cluster, image and HOME carry a random per-run id, so runs from two checkouts never collide. It stops when a setup capture that later captures depend on fails, such as `server add`.
- [cloud-cluster/run.sh](cloud-cluster/run.sh): needs a paired run from `scripts/verify-cluster up --machines 1 --daemon checkout --cloud`, linked to `shop` and deployed as its header says; call it as `MANIFEST=…/cluster.json SECTIONS="deploy runtime domain env errors up" run.sh [PARENT]`. Without `MANIFEST` it takes the newest run, and without `SECTIONS` it runs all six. `up` appends to `deploy.md`, so it needs `deploy` earlier in the same run. Cloud asks a person to approve a removal from `ployz`, so for env rm, project rm and the deploys that remove a domain (including the old host when the generated domain changes) the driver approves, as the seed's owner Ada, the approval id the command itself prints, with `ployz org approval approve` on a terminal because it asks for the Organization's name ([approve.sh](approve.sh)). It refuses a run whose manifest lacks a `.cloud` block or whose `state` is not `ready` (a torn-down run keeps its manifest as `removed`), and one that does not answer.

- [record.py](record.py): runs one command in a PTY and writes an asciicast, a GIF (`agg`) and a last-frame PNG (`ffmpeg`), as in `record.py --gif out.gif --png out.png -- 'ployz deploy'`. It writes nothing unless the command exits with `--expect-exit` (default 0), and replaces each output through a temp file and a rename. [prototype-media/make.sh](prototype-media/make.sh) uses it to re-record the prototype GIFs.
- [cap.sh](cap.sh): appends one command's capture to a `.md` and its terminal rendering to the matching `.ansi`. Every driver captures through it. `CAP_STDERR=FILE` writes the piped run's stderr to FILE as it happens, so a caller can read a line the command prints before it exits.
- [approve.sh](approve.sh): the cloud and cloud-cluster drivers source it to capture a command Cloud asks a person to approve. It approves only the id that command prints in its own `next: ployz org approval approve <id>` line. If no single id appears within 120s, the approver doesn't finish within `APPROVE_TIMEOUT` (60s), or the command then fails, the run stops without `.ok`. `bash approve.test.sh` checks that against a stub `ployz` with two pending ids, a failing command and a stalled approver.
- [capture-dir.sh](capture-dir.sh): creates a driver's fresh output directory under PARENT, so two drivers never share one, and refuses a PARENT inside a git checkout.
- [publish.sh](publish.sh): copies a finished driver run over the tracked captures, as drawn above.
- [scrub.sh](scrub.sh): the scrub `publish.sh` runs. Public IPv4 addresses become `203.0.113.10` and global IPv6 addresses `2001:db8::10`, because Servers report their public IP, and secrets (`ployz_` tokens, `pmet_` enrollment tokens, `ppair_` pairing secrets) keep their prefix and become `<prefix>_<redacted>`. `bash scrub.test.sh` checks it after a change.

Each `.md` holds one block per command with its exit code, stdout and stderr. Each `.ansi` holds the same commands run under a terminal. Exit 124 means the capture's own timeout killed the command.

Totals: 684 command blocks (store 244, cloud 155, cluster 105, cloud-cluster 180). Exits: 0 x394, 1 x173, 2 x91, 3 x9, 124 x7, 127 x1, 7 x1, none x8 (piped, typed TTY or killed runs).

## The contract as the code states it

From `core/crates/ployz/src/ui/result.rs` and `ui/exit.rs`:

```
                  without --json               with --json
result            stdout (human text)          stdout: one JSON object
progress          stderr                       stderr
! warnings        stderr                       stderr, and copied into "warnings"
error             stderr: error:, cause:,      stdout: {"error":{cause,code,details,message}}
                    then hint lines
hints             stderr: next:, retry:, …     keys of the result or of the error's details
streaming         stdout lines                 stdout: one JSON object per line
prompts           only on a TTY (stdin+out)    never
```

- `finish` prints the JSON value or runs the human closure. `list` prints a header TSV table; an empty one keeps its header on stdout and says so on stderr. `fields` prints `key = value` lines, `done` one sentence, and `show` pretty JSON in both modes.
- `warn` prints `! …` on stderr and adds the text to the next JSON result's `warnings`.
- Fan-outs print `{…, failures, omitted}`, warn `! <Server> did not answer; its rows are missing.` for each gap, and exit 3 if either list is non-empty.
- After a result is printed, any later error goes to stderr and the exit becomes 3, unless the printed result is itself the failure: a failed deploy exits 1.
- Human errors print `error: <message>`, then `  cause: <deepest cause>`, then hint lines: `next:`, `inspect:`, `retry:`, `undo:` and `valid:`.
- Exit codes: 0 success; 1 any command failure; 2 when the error code is `invalid_argument`, `confirmation_required` or `ambiguous` (`exit_code`); 3 partial (`PARTIAL_EXIT`, "some Servers failed or did not answer"); 130 interrupted. `ployz exec` passes through the remote exit code.

## 1. Lists and tables

Dominant: tab-separated rows under an UPPERCASE header. 53 header lines across the captures, in 13 kinds (SERVICE 12, ENVIRONMENT 8, DOMAIN 7, DEPLOYMENT 6, SERVER 4, NAME 3, ORGANIZATION 3, KIND 2, PROJECT 2, REPOSITORY 2, VOLUME 2, NAMESPACE 1, SETTING 1).

- Header TSV. `SERVICE	PRIVATE DNS	SOURCE	NEXT DEPLOY` ([store/service.md](store/service.md)). The current row is marked in a `CURRENT` column: ctx ls prints `prod	current	unix:///tmp/prod.sock	2` ([store/context.md](store/context.md)), and org ls has `ORGANIZATION	NAME	CURRENT` ([cloud/org.md](cloud/org.md)).
- server ls has 7 columns: `SERVER	MEMBERSHIP	STORAGE	PUBLIC IP	SUBNET	DAEMON	OS` ([cluster/server.md](cluster/server.md)). ps has 4: `SERVICE	KIND	SERVER	STATE` ([cluster/ps-logs-exec.md](cluster/ps-logs-exec.md)).
- deployment ls: `DEPLOYMENT	STATUS	SAVED REVISION`, then rows like `#5	queued	2` ([cloud/deploy.md](cloud/deploy.md)).
- `key = value` records. get: `web.replicas = 2 (default 1)`, with `- (default)` for unset values ([store/settings.md](store/settings.md)). status, server show, service inspect and explain print the same shape.
- Log lines. `2026-10-20T16:19:53.335329562+00:00 gamma web/b966ab89d05c | tick` ([cluster/ps-logs-exec.md](cluster/ps-logs-exec.md)). Server logs drop the fractional seconds: `2026-10-20T16:16:07+00:00 alpha ployz | ployz testkit journal entry` ([cluster/server.md](cluster/server.md)). `-n` counts lines for the whole command, not per container: `logs web -n 3` printed 3 lines from two containers, and `logs -n 2` printed 2. Lines follow the container's stream, so nginx notices land on stderr ([cloud-cluster/runtime.md](cloud-cluster/runtime.md)).

Empty lists keep the header on stdout and say so on stderr, usually with a `next:` line:

- `PROJECT	DEFAULT ENVIRONMENT	ENVIRONMENTS`, then `No Projects yet.` and `next: ployz project new NAME` ([store/project.md](store/project.md)).
- `No Services in production yet.` with `next: ployz service add NAME --image REF --project shop` ([store/service.md](store/service.md)).
- `No contexts yet. Contexts reach Clusters outside Ployz Cloud; your Cloud Servers need none.` with `next: ployz login` ([store/context.md](store/context.md)).
- get with no Services prints only `No Services in shop/production yet.`, with no header ([store/settings.md](store/settings.md)). service ls names the scope `production`, get `shop/production`.
- Nothing staged reads two ways, on two streams: diff says `No staged changes in empty/production.` on stderr, and `deploy --skip-deploy` says `Nothing staged in empty/production.` on stdout ([store/staging.md](store/staging.md)).

## 2. Success and confirmation lines

Dominant: one past-tense sentence naming the object and its scope, ending with a period. Of the 95 human success sentences that open with a past-tense verb or `Nothing`, 94 end with a period. The exception is token new's line, which ends with a colon before the secret.

- Staged. `Staged web.replicas in shop/production.` 36 lines take this shape ([store/settings.md](store/settings.md)). Multi-object forms print `Staged: worker, configs.sentry` on a second line ([cloud-cluster/env.md](cloud-cluster/env.md)).
- Made or removed. `Created Project shop with Environment production.` and `Removed Project scratch1 and its Environments (production).` ([store/project.md](store/project.md)), `Revoked token eaf5a551-4ef7-4b40-a6c4-b906216ee3da.` ([cloud/token.md](cloud/token.md)).
- Nothing to do. `Nothing changed in shop/production; web.env.MISSING is already unset.` ([store/settings.md](store/settings.md)), `Nothing changed on the Servers; Deployment #2 shipped version #1.` ([cloud-cluster/deploy.md](cloud-cluster/deploy.md)).
- One line per container. `Restarted shop-production/web on alpha.` then `Restarted shop-production/web on gamma.` ([cluster/service.md](cluster/service.md)).
- A removal through Cloud. env rm prints `Removed Environment shop/hotfix.` and project rm `Removed Project workspace and its Environments (production).`, the same sentences as the local Store, after the removal Deployment's progress on stderr ([cloud-cluster/env.md](cloud-cluster/env.md), [cloud-cluster/deploy.md](cloud-cluster/deploy.md)).
- A secret. token new prints the secret alone on stdout, and `Made token ci (Full as you) in Organization ada, expiring 2027-01-18T09:50:28.004Z. Its secret is shown only now; set it as PLOYZ_TOKEN:` on stderr ([cloud/token.md](cloud/token.md)).
- Status block. `organization = local` / `signed in as = local` / `environment = shop/production` / `staged = 13 changes`, then `next: ployz diff --project shop` ([store/context.md](store/context.md)).
- Port forward. `Forwarding 127.0.0.1:37333 to shop-production/web (ployz-create-…) port 80.` on stdout, then `Ctrl-C stops it.` on stderr ([cluster/service.md](cluster/service.md)).

Change arrows are `→` everywhere: diff and `deploy --plan` print `web.env.GREETING: null → "hello"` ([cloud-cluster/deploy.md](cloud-cluster/deploy.md)), and env sync prints `Synced fix-web → shop/production.` ([store/env.md](store/env.md)). A plan ends with `Decided by the Servers when it runs: which containers start and stop.`

## 3. Progress and following

One follower, two looks. Piped, it prints plain lines on stderr: a heading, a line each time a node changes, a rule and a summary.

```
Deploying #5 of shop/production
  api (depth 0): unchanged
  postgres (depth 0): unchanged
  redis (depth 0): unchanged
  web (depth 1): waiting 0/1
  worker (depth 1): unchanged
  Volume pg-data: unchanged
  Config sentry: pending
  web (depth 1): booting 0/1
  web (depth 1): switching 1/1
  web (depth 1): switched 1/1
  Config sentry: unchanged
──────────────────────────────────────────
1 deployed · 6 unchanged · across 1 server
```

([cloud-cluster/deploy.md](cloud-cluster/deploy.md), warnings left out). In the piped list, Services come by depth, then name, and the Volume and the Config follow. On a TTY the same follower redraws in place with marks and timings, `✔ Volume pg-data  unchanged`, `depth 1`, `  ✔ web  switched 1/1  14.0s`, and marks a failed node `✘` ([cloud-cluster/deploy.ansi](cloud-cluster/deploy.ansi)). Warnings print between the node lines as they arrive. Under `--json` the lines stay on stderr. `--detach` prints a result instead, `Deployment #7 of shop/production: queued` and a `  api: pending` line per node ([cloud-cluster/deploy.md](cloud-cluster/deploy.md)). `--events FILE` writes NDJSON like `{"id":"…","nodes":[…],"number":3,"status":"queued","type":"deployment"}` ([cluster/deploy.md](cluster/deploy.md)). The local Store prints the same heading ([store/errors.md](store/errors.md)). `up` adds steps before the follower, `Created Project workspace.`, `Added Service workspace.` and build lines such as `  workspace: waiting (building on machine-1)`, ends with `workspace built on machine-1.`, and prints `Open https://workspace.ada.ployz.test` and `Dashboard: http://localhost:…` on stdout ([cloud-cluster/deploy.md](cloud-cluster/deploy.md)).

server add follows the ingress the same way, `Deploying ingress to default` on the first Server and `Starting Globals on beta` on the next, then `Server beta is ready.` ([cluster/server.md](cluster/server.md)).

Other waits print one line and block, and their ellipses differ. login prints `Open http://localhost:35249/device?user_code=… and confirm the code ….` then `Waiting for approval... ` ([cloud/auth.md](cloud/auth.md)). An approval prints `Waiting for approval in Ployz Cloud… ` ([cloud-cluster/env.md](cloud-cluster/env.md)). server add prints `Waiting for ssh://root@172.26.0.2 to participate: …; retrying for up to 295s. Check outbound firewall access if this connection is blocked.` ([cluster/server.md](cluster/server.md)).

## 4. Prompts and confirmations

Dominant: no prompt off a terminal. A destructive command without `--confirm` refuses (exit 2, `confirmation_required`), names the loss and prints the retry:

```
error: Removing Project scratch1 deletes every Environment in it (production) with its configuration, history, Services and Volumes; this can't be undone. No changes made.
retry: ployz project rm scratch1 --confirm scratch1
```

([store/project.md](store/project.md)). On a TTY, project rm asks instead, `Type blog to continue:`, and Enter alone ends with `Cancelled. Nothing was removed.`

- Wrong value. `--confirm scratch does not match Project scratch1. No changes made.` also exits 2 ([store/project.md](store/project.md)). Same for org rm and env rm.
- What you type differs: `--confirm scratch1` for a Project, `--confirm shop/qa` for an Environment ([store/env.md](store/env.md)).
- Volume loss. `This permanently deletes the data of store. Accept each by name` (no period), then `retry: ployz deploy --accept-volume-loss store --expect-version 16:7:0.10:3db84a7b2f1973e4` ([cluster/volume.md](cluster/volume.md)).
- `--yes`. cloud reset refuses with `…then confirm with --yes` and `retry: ployz cloud reset --yes` ([cloud/deploy.md](cloud/deploy.md)).
- Picker on a TTY. ctx use draws `Select a context:` over `  dev` and `> prod (current)`, moved with the arrow keys, On `<Down> <Enter>` it ends `Current context is now dev.` A typed `9` is ignored, and the picker keeps the current context, dev ([store/context.md](store/context.md)). Without a TTY: `Choosing a context needs a terminal; name one instead.` with `retry: ployz ctx use CONTEXT` and `valid: dev, prod`.
- Cloud approval. A removal from `ployz` waits for a person: `This removal destroys 2 things:`, a `  ✕ removes Service api` line each, `Approve <id> in Ployz Cloud at http://localhost:…`, `next: ployz org approval approve <id>` and `Waiting for approval in Ployz Cloud… `. A project rm or org rm points at the Ployz agent instead, `Approve <id> under Waiting on you in the Ployz agent at …`. Once Ada approves, the line ends `approved.` and the removal runs ([cloud-cluster/env.md](cloud-cluster/env.md), [cloud-cluster/deploy.md](cloud-cluster/deploy.md), [cloud/org.md](cloud/org.md)).

## 5. Warnings

Dominant: `! <sentence>.` on stderr. 35 lines, 12 distinct texts.

| Count | Text (start) | Capture |
|---|---|---|
| 7 | `! gamma did not answer; its rows are missing.` | [cluster/errors.md](cluster/errors.md) |
| 7 | `! acme.com answers from another server. Point it at 2001:db8::10. …` | [cloud-cluster/deploy.md](cloud-cluster/deploy.md) |
| 6 | `! web.ada.ployz.test is not published yet. Ployz Cloud publishes it once it reaches port 80 on 2001:db8::10.` | [cloud-cluster/deploy.md](cloud-cluster/deploy.md) |
| 5 | `! Docker only (not recommended): Volumes on this Server get no size limits, …` | [cluster/server.md](cluster/server.md) |
| 2 | `! Server gamma is running Services: shop-production/web.` | [cluster/server.md](cluster/server.md) |
| 2 | `! workspace.ada.ployz.test is not published yet. …` | [cloud-cluster/deploy.md](cloud-cluster/deploy.md) |
| 1 each | 6 more: two more unpublished domains, `! api's image traefik/whoami:v1.11.0 exposes 80; its domains send to 8080. …`, `! Replicated Services may now be under-replicated: …`, `! Docker volume (not recommended): …` and `! No Server here can host Managed volumes yet, …` | [cloud-cluster/domain.md](cloud-cluster/domain.md), [cluster/server.md](cluster/server.md), [cluster/volume.md](cluster/volume.md) |

Every warning starts uppercase or with a name. Under `--json` the same text also appears in a `warnings` array.

## 6. Errors

Human errors go to stderr as `error: <message>`. 185 human error lines and 78 JSON error objects were captured, and 12 human errors add a `  cause:` line.

First-word casing: 127 start uppercase, 36 lowercase, 19 are clap's, and 3 start with `--confirm`.

- Dominant, uppercase: `No Service named nope in Environment production` then `valid: web, db` ([store/service.md](store/service.md)). Most end without a period; some end with one, such as `Not signed in to Cloud.` ([cloud/errors.md](cloud/errors.md)).
- Lowercase: `no Organization nope of yours` ([cloud/org.md](cloud/org.md)), `context nope not found in Ployz config …` ([store/context.md](store/context.md)), `at least one setting flag is required` ([cluster/server.md](cluster/server.md)). Most lowercase lines lead with a name: `staging is not a Branch`, `shop/production has no Deployment #99`, `worker: pull image failed.`
- Clap: `error: unexpected argument '--bogus' found`, a `tip:` line and a Usage line ([store/errors.md](store/errors.md)). Under `--json` the whole clap text, Usage included, goes into `message` with code `invalid_argument`.

Not-found wording varies for the same idea:

- `No Service named nope in Environment production` ([store/service.md](store/service.md))
- `Could not show Server nope.` with `  cause: No Server named "nope"` and `inspect: ployz server ls` ([cluster/errors.md](cluster/errors.md))
- `Server nope was not found` from server drain ([cluster/errors.md](cluster/errors.md))
- `No such domain in Environment production` ([cloud-cluster/errors.md](cloud-cluster/errors.md))
- `shop/production has no Deployment #99` ([cloud-cluster/errors.md](cloud-cluster/errors.md))

Raw upstream text mostly moved to the `cause:` line:

- `Could not reach Cloud at http://127.0.0.1:9.` with `  cause: Connection refused (os error 111)` ([cloud/errors.md](cloud/errors.md))
- `Could not start the command in ployz-create-….` with `  cause: Docker responded with status code 409: container … is not running` ([cloud-cluster/errors.md](cloud-cluster/errors.md))
- `worker: pull image failed.` with `  cause: Docker responded with status code 500: error from registry: denied; denied` ([cloud-cluster/deploy.md](cloud-cluster/deploy.md))
- `Ployz couldn't reach your Server's daemon.` with `  cause: SSH connection to root@10.255.255.1 timed out after 3 seconds; a slow or distant Server may need --ssh-timeout 60` ([cluster/server.md](cluster/server.md))

Some still carry it in the message:

- `Cloud answered HTTP 405` as `invalid_argument` ([cloud/errors.md](cloud/errors.md))
- `Server gamma failed to upgrade to 0.2.3 while launching: could not inspect the Machine upgrade worker: No such file or directory (os error 2)`, then `inspect: journalctl -u ployz-upgrade-….service`. The testkit has no systemd, so the upgrade fails at launch ([cluster/server.md](cluster/server.md)).
- `Server 7f00d39ce93641bd8c5fdd7d91395aa7 isn't answering. Check that its Ployz daemon is running, then retry: …`, inside server add's wait line, by Machine ID because the Server has no name there yet ([cluster/server.md](cluster/server.md)). Pinned by `cli_shape::a_server_cloud_handed_a_connection_for_that_does_not_answer_is_named`.
- `Couldn't check ingress: Server 0123456789abcdef0123456789abcdef isn't answering (Connection refused (os error 111)).` on `deploy --plan`, and as the deploy's `! Couldn't check ingress: … Deploy again to retry.` warning. No capture yet: pinned by `config_cli::an_agent_adds_checks_and_removes_domains` and `ingress::tests::an_unchecked_ingress_names_the_server_and_why`.

## 7. Follow-up hints

Dominant: a trailing `next: <command>` line. Every hint is a line of its own, labelled except the near miss `did you mean NAME?`.

| Form | Count | Example | Capture |
|---|---|---|---|
| `next:` | 142 in 22 files | `next: ployz login` | [cloud/errors.md](cloud/errors.md) |
| `valid:` | 35 in 11 files | `valid: api, postgres, redis, web, worker` | [cloud-cluster/errors.md](cloud-cluster/errors.md) |
| `inspect:` | 14 in 8 files | `inspect: ployz server ls` | [cluster/errors.md](cluster/errors.md) |
| `retry:` | 13 in 9 files | `retry: ployz project rm scratch1 --confirm scratch1` | [store/project.md](store/project.md) |
| `undo:` | 9 in 4 files | `undo: ployz history undo 7 --project shop --env production` | [cloud-cluster/deploy.md](cloud-cluster/deploy.md) |

- `next:` targets vary in scope: `ployz diff --project shop`, `ployz diff`, `ployz deploy --project shop`.
- Human output prints fewer hints than the JSON carries. 53 of 126 JSON success objects have a top-level `next`. 86 of 258 human successes print `next:`.
- `valid:` shows eight choices, then `and N more`: `valid: cpuLimit, healthcheck, image, maxRetries, memLimit, preDeployCommand, privateDns, registryCredential, and 18 more` ([store/settings.md](store/settings.md)). A near miss adds an unlabeled line above it: `did you mean replicas?`.

## 8. Identifiers

Dominant: names (Project, Environment, Service, Server, Volume, Organization). Machine-shaped ids still show up in places:

- UUIDs. token rm takes only the token's UUID: `token rm ci` fails with `no token or signed-in device ci in this Organization` ([cloud/token.md](cloud/token.md)).
- Deployments have a number and a UUID, and `#N` also numbers saved versions: `Nothing changed on the Servers; Deployment #2 shipped version #1.` ([cloud-cluster/deploy.md](cloud-cluster/deploy.md)), `Saved version #1 of shop/production. Not deployed.` ([store/staging.md](store/staging.md)). A bad id exits 2 with `Expected a Deployment ID or number` ([cluster/deploy.md](cluster/deploy.md)).
- 32-hex Machine ids. server show prints `id = 91e8bbd81cd34c59a186d49d3375bbe8` ([cluster/server.md](cluster/server.md)). A Server Cloud holds no name for goes by its id: `Server 0123456789abcdef0123456789abcdef couldn't give this command access: invalid management connections` ([cloud/deploy.md](cloud/deploy.md)).
- Container ids. 12 hex in log lines (`web/b966ab89d05c`), 64 hex in JSON `container_id` ([cluster/ps-logs-exec.md](cluster/ps-logs-exec.md)). Human text names a container `ployz-create-<64 hex>` in port-forward and in exec on a stopped Service, whose cause then names a second 64-hex id ([cloud-cluster/errors.md](cloud-cluster/errors.md)). These captures predate #2560: main names a new keyed container after its Service, `<service>-<12 hex>`.
- One Service, two ids. JSON has a UUID `id` and a hex runtime `service_id` ([cloud-cluster/runtime.md](cloud-cluster/runtime.md)).
- Version strings. `14:1:0.1`, `16:7:0.10:3db84a7b2f1973e4`, `31:e25e4c887592ac4e` ([cloud-cluster/deploy.md](cloud-cluster/deploy.md), [cluster/volume.md](cluster/volume.md), [store/env.md](store/env.md)).
- Runtime names. `shop-production/web` in success lines and warnings, `shop/production` in prose.

## 9. Partial results and unreachable Machines

Captured with gamma stopped ([cluster/errors.md](cluster/errors.md)).

| Command | Exit | What it says |
|---|---|---|
| ps, ps --json | 3 | `! gamma did not answer; its rows are missing.`; the JSON lists gamma under `omitted` |
| server ls, server ls --json | 3 | the same warning; gamma's row says `suspect` with STORAGE `Volume support unknown` |
| service restart | 3 | `Restarted shop-production/web on alpha.`, then the same warning |
| logs, server logs | 3 | the other Servers' lines, then the same warning |
| server show gamma | 1 | `Could not show Server gamma.` with `  cause: target Machine RPC timed out` |

A failed Deployment exits 1, with its progress and the error on stderr: `Deploying #1 of shop/production`, a line per node such as `  db: not started` and `  Volume pgdata: not attempted`, a rule, `3 not attempted`, then `error: No Server is enrolled in this Organization.` and a `retry:` line ([store/errors.md](store/errors.md)). A Deployment that applied but could not see every Server, or that a newer one superseded or someone cancelled, exits 3.

## 10. Human output that is really JSON

- schema prints a 6629-line JSON Schema with or without `--json` ([store/settings.md](store/settings.md)).
- JSON inside human lines: `web.healthcheck = {"path":"/healthz","timeoutSeconds":300} (default -)` and `web.env.API_KEY = {"secret":true}` ([store/settings.md](store/settings.md)). diff values are JSON literals: `web.env.GREETING: null → "hello"` ([cloud-cluster/deploy.md](cloud-cluster/deploy.md)).

## 11. `--json` shape

Dominant: one pretty-printed object on stdout (126 of 130 success outputs). The other 4 are NDJSON streams (logs and server logs).

- Errors are compact, on one line: `{"error":{"cause":[],"code":"not_found","details":{},"message":"No Project named nope"}}`. Codes seen: not_found 24, invalid_argument 20, unauthenticated 15, conflict 10, unavailable 5, confirmation_required 3, ambiguous 1.
- Mutations share `environment`, `staged` and `next` ([store/settings.md](store/settings.md)).
- Fan-outs add `failures` and `omitted` ([cluster/errors.md](cluster/errors.md)).
- Time fields come in three forms: epoch seconds (`admitted_at`), ISO strings (`expires_at`), and `created_at_unix_nanos` ([cluster/ps-logs-exec.md](cluster/ps-logs-exec.md)).
- Keys are snake_case except `canRestore` ([cloud-cluster/deploy.md](cloud-cluster/deploy.md)) and Setting names, which keep their camelCase (`timeoutSeconds`).
- Secrets: ps --json shows env values as `<redacted>`. `deploy --plan --json` dumps full container specs under `preview` ([cluster/deploy.md](cluster/deploy.md)).
- 16 `--json` runs that exit 0 also printed non-warning human text on stderr: the deploy follower, `Restarted shop-production/web on machine-1.` and the other per-container lines, `Added Server gamma.`, domain ls's `Add DNS for acme.com: AAAA @ 2001:db8::10` and others. stdout stayed one object.

## 12. Exit codes

| Exit | Count | Meaning seen |
|---|---|---|
| 0 | 394 | success |
| 1 | 173 | any failure: not found, conflict, unauthenticated, unavailable |
| 2 | 91 | clap errors, a missing or wrong `--confirm`, volume loss, `Expected a Deployment ID or number`, local `deploy --detach`, two Projects and none chosen |
| 3 | 9 | partial fan-out with gamma down (7), `server upgrade` failing on its one Server, and `ployz exec` passing a remote 3 through |
| 124 | 7 | the capture's own timeout: `logs --follow` (3); in cloud/, a login (2), `github connect`, and a `deploy` follow that never ends |
| 7, 127 | 2 | `ployz exec` passing through the remote code ([cloud-cluster/runtime.md](cloud-cluster/runtime.md), [cloud-cluster/errors.md](cloud-cluster/errors.md)) |

8 blocks record no exit of their own: three typed TTY runs, three piped through `head`, and two port-forwards whose `# exit 124` lands in their stderr. Exit 2 now follows the error code, so a missing `--confirm` and a wrong one both exit 2. `ployz exec` can return 1, 2 or 3 from the remote command, which overlap ployz's own codes ([cluster/ps-logs-exec.md](cluster/ps-logs-exec.md)).

## 13. Help text

- Bare `ployz`, `ployz org` and `ployz github` print help on stdout and exit 0 ([store/help.md](store/help.md), [cloud/org.md](cloud/org.md), [cloud/github.md](cloud/github.md)).
- Summaries mix casing: `server      Manage Servers` but `service     Manage services` ([store/help.md](store/help.md)).
- Commands list the groups first (`cloud` to `volume`), then the verbs (`build` to `up`), each alphabetically, then `help`.
- Every subcommand repeats the globals: `--connect <connect>`, `--ssh-timeout <SECONDS>`, `-c, --context <context>`, `--json` and `--color <WHEN>`.
- Placeholders mix casing: `<name>`, `<context>`, `<connect>`, `<SECONDS>`, `<WHEN>`, `<REF>`.
- project rm help leaves `<name>` with an empty description.
- Top-level help ends with a `Settings catalog: …` line. `--version` prints `0.3.0-beta.6`.

## Likely bugs

Each item reproduces in a capture on main, at the line it cites.

1. `exec` of a missing binary prints `OCI runtime exec failed: …` on stdout, exit 127. [cloud-cluster/errors.md:31](cloud-cluster/errors.md) **Kept: `exec` passes the remote command's output and exit code through.**
2. project rm help leaves the `<name>` argument with an empty description. [store/help.md:403](store/help.md)

The earlier list's 32 items, rechecked against the same captures. Old item 23 is item 1 above.

Fixed, each at the capture that shows it:

- 1, `unset web.env.MISSING` claimed `already set`: now `Nothing changed in shop/production; web.env.MISSING is already unset.` [store/settings.md:978](store/settings.md)
- 2, an empty save claimed it published: `deploy --skip-deploy`, the save today, says `Nothing staged in empty/production.` [store/staging.md:621](store/staging.md)
- 3, an empty discard claimed it discarded: now `Nothing staged in empty/production; nothing to discard.` [store/staging.md:585](store/staging.md)
- 6, local `deploy --detach` left a queued Deployment: it refuses, and `deployment ls` shows none queued. [cluster/deploy.md:528](cluster/deploy.md), [cluster/deploy.md:540](cluster/deploy.md)
- 7, server ls with gamma down said nothing: it warns `! gamma did not answer; its rows are missing.` [cluster/errors.md:55](cluster/errors.md)
- 8, a missing `--confirm` exited 1: both a missing and a wrong one exit 2. [store/project.md:224](store/project.md), [store/project.md:249](store/project.md)
- 9, server upgrade printed its failure twice, then the cause again: one `error:` line and one `inspect:` line. [cluster/server.md:852](cluster/server.md)
- 10, `domain add` of an existing domain claimed it staged: now `Nothing changed in shop/production; acme.com is already on web.` [cloud-cluster/domain.md:604](cloud-cluster/domain.md)
- 11, `env shutdown` of a never-deployed Environment printed a bare `applied: `: now `Deployment #1 of shop/qa: applied`. [store/env.md:988](store/env.md)
- 12, `cannot Select a context…`: now `Choosing a context needs a terminal; name one instead.` [store/context.md:514](store/context.md)
- 13, signed-out ps and logs reported a socket error: now `Not signed in to Cloud.` with `next: ployz login`. [cloud/errors.md:198](cloud/errors.md), [cloud/errors.md:223](cloud/errors.md)
- 14, the server rm retry quoted `~`: now `retry: ployz server rm gamma --context default --confirm gamma`. [cluster/errors.md:40](cluster/errors.md)
- 15, server rm removed first, then warned, and printed an empty `Volumes the Cluster loses (0)` list: it warns before it removes (`handlers/server/remove.rs`), and no empty list prints. The capture shows stdout and stderr apart, so their order on the page is not the order they ran in. [cluster/server.md:1009](cluster/server.md)
- 16, service restart warned twice: one warning. [cluster/errors.md:644](cluster/errors.md)
- 17, logs exited 0 with a Machine down: exit 3 and a warning. [cluster/errors.md:609](cluster/errors.md)
- 18, server drain `--json` printed `public_key` as bytes: a base64 string. [cluster/server.md:897](cluster/server.md)
- 19, `server show nope --json` said `invalid_argument`: now `not_found`. [cloud-cluster/errors.md:483](cloud-cluster/errors.md)
- 20, a registry error's newline broke the result: the cause is one line, `error from registry: denied; denied`. [cloud-cluster/deploy.md:1436](cloud-cluster/deploy.md)
- 21, up printed `built ` with no detail: now `workspace built on machine-1.` [cloud-cluster/deploy.md:3066](cloud-cluster/deploy.md)
- 22, clap Usage showed `--ployz-config`: now `Usage: ployz service add --image <REF> --project <project> <name>`. [store/service.md:162](store/service.md)
- 24, status for a deleted Project gave no repair step: now `needs attention = No Project named workspace` with `next: ployz link --project PROJECT`. [cloud-cluster/deploy.md:3295](cloud-cluster/deploy.md)
- 25, the empty `--secret` error carried a postgres `example`: its details are `null`. [store/settings.md:105](store/settings.md)
- 26, `env branch --live` had two colons: now `db can't be used live: the Branch copies it, so nothing running can lend it`. [store/env.md:936](store/env.md)
- 28, server add over SSH ended on its wait line: it follows the ingress and ends `Server beta is ready.` [cluster/server.md:27](cluster/server.md)
- 30, human service add omitted `next`: it prints `next: ployz deploy --project shop`. [store/service.md:43](store/service.md)
- 4, `org ls` after removing the acting Organization: org rm prints `This device now acts in Organization ada.`, and `org ls` lists `ada	Ada Lovelace's Projects	current`. [cloud/org.md:264](cloud/org.md), [cloud/org.md:286](cloud/org.md)
- 32, bare `ployz org` printed help on stderr with exit 2: stdout, exit 0. [cloud/org.md:48](cloud/org.md)

Intentional, per a rule the captures state:

- 5, a Managed volume can't switch storage after a Deploy was requested: `Volume cache keeps its storage once a Deploy has been requested; add a new Volume for other storage`. [cluster/volume.md:229](cluster/volume.md)
- 27, a malformed `--expect-version 1` exits 2 and a stale well-formed one exits 1. Exit 2 means the command line is wrong; a stale version is a conflict with the current state, which exits 1. [cloud/deploy.md:768](cloud/deploy.md), [cloud-cluster/errors.md:107](cloud-cluster/errors.md)

Fixed on main after this list was drawn up:

- 29, `deployment start 1` with no Server followed until the capture's timeout killed it (exit 124), and printed nothing. Since #2634 it refuses at once, as a Deploy does: `This Organization has no servers yet, so nothing deployed.` with `next: ployz server add`. The seed enrolls a Server that never answers, so the capture sets its row aside for this one start. [cloud/deploy.md:310](cloud/deploy.md)
- `server upgrade 0.2.3 gamma` exited 3, the partial code, with nothing on stdout while its only Server failed. Since #2630 it names the Server on stdout: `Server gamma failed to upgrade to 0.2.3 while launching.` It still exits 3. [cluster/server.md:852](cluster/server.md)
- env rm's removal Deployment printed `  api: starting 0/1` before `  api: removing 0/1` in an earlier capture. #2629 shows a begun removal as removing, and the recapture removes api without starting it. [cloud-cluster/env.md:573](cloud-cluster/env.md)

Not rechecked, because no capture reaches it now:

- 31, the `Docker only` prompt's warning placement: no capture shows that prompt.

## Open design questions

Five earlier questions are settled in the code, each as its option B or close to it:

- Exit codes for usage mistakes: exit 2 follows the error code, set in one place (`exit_code`).
- Hint vocabulary: `next:`, `inspect:`, `retry:`, `undo:` and `valid:`, lowercase, each on its own line.
- List format: header TSV for lists, `key = value` for single objects; an empty list keeps its header.
- Partial results: every fan-out warns `! <Server> did not answer; its rows are missing.` and exits 3.
- Progress rendering: plain lines when piped, a redraw on a TTY, for deploys and server add alike.

Still open:

1. **Should human output print every `next` the JSON carries?** 53 JSON successes carry `next`; 86 human ones print it.
   A. Yes, always.
   B. Only when the next step is not obvious (for example after a stage, say "deploy").
   C. Never on success; hints only on errors.

2. **Hex ids in human output.** Machine ids name Servers Cloud holds no name for, and 64-hex container ids appear in port-forward and exec errors in these captures (since #2560, new keyed containers are named `<service>-<12 hex>`).
   A. Names only; ids live in `--json` and inspect.
   B. Names first, with a short id where two things can share a name.
   C. Keep ids where they appear today.

3. **Error text.** 127 errors start uppercase and 36 lowercase, most of those with a name. Most raw upstream text now sits on a `cause:` line, but some still reaches the message.
   A. One sentence in our words, capitalized, no period; the raw cause only in JSON `details`.
   B. Our sentence, then `cause: …` on a second line with the raw text.
   C. Keep passing the raw text through, but fix casing.

