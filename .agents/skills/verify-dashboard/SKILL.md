---
name: verify-dashboard
description: Run a seeded, signed-in Ployz Cloud dashboard for this checkout and prove a UI change works in a real browser. Use after changing dashboard/ UI, routes, loaders or Config Store writes, when asked to verify, screenshot or click through Ployz Cloud, or when a fresh worktree needs the dashboard running.
---

# Verify the dashboard

One command gives this checkout its own Postgres, seed and `vite dev`, signed in as Ada Lovelace. Checkouts run side by side. Drive it with `agent-browser`. Run commands from `dashboard/`.

## Launch

Pick the mode from what the change shows. UI that reads a running Server (runtime status, Logs, metrics, Monitoring, deploy progress, upgrades, Volume copies) needs `SERVERS=1`; one Server is enough unless the screen compares Servers. Without it those screens stay empty, so sample data on a page is not proof. Plain `up.sh` covers everything that reads only the Config Store.

```bash
scripts/verify/up.sh          # free port; BILLING=1 scripts/verify/up.sh turns billing on
scripts/verify/up.sh --fresh  # restart vite and Postgres instead of reseeding in place
SERVERS=2 scripts/verify/up.sh   # also real Servers paired through Cloud; DAEMON=stable (default), beta, checkout or a version
FAKE_SERVERS=1 scripts/verify/up.sh   # no real Servers; the Servers list shows fra-1, fra-2, syd-1 and ams-1 from a fixture
FAKE_GITHUB=1 scripts/verify/up.sh    # Cloud talks to a fake GitHub; connecting a repository works end to end
```

With `SERVERS`, `up.sh` also starts an Inngest dev server and the worker for this checkout, then runs `../core/scripts/verify-cluster` (see [verify-server](../verify-server/SKILL.md)). That command enrolls the Machines through this dashboard with an Organization Token. Deploys and enrollment follow-ups then really run. `.verify/run/info` adds the Inngest UI URL for function runs and the `verify-cluster cli` line for the cluster. `DAEMON` is a space-separated list of `[machine-N=]SOURCE` choices passed to `verify-cluster up --daemon`, so `DAEMON="checkout machine-2=0.2.3"` runs machine-1 on this checkout and machine-2 on the 0.2.3 release. Every choice enrolls. A release Machine that cannot enroll with this checkout's CLI enrolls on the checkout daemon first and then gets its release, so `stable` off a release commit compiles the daemon. The Machines are Boat sandboxes by default; the first run of a new recipe bakes their snapshot, which takes a few minutes.

A fresh worktree is fine: `up.sh` installs `node_modules` and builds the native SDK when either is missing or stale. The SDK build is shared across worktrees through `~/.cache/ployz/sdk`, keyed by the committed Rust sources, so only the first checkout of a given commit compiles it (about 2 minutes). A warm run takes about 40 seconds on a busy host. Rerunning it in the same mode reseeds in place, except with `SERVERS` or `FAKE_GITHUB`, which always restart: it empties the tables and seeds again while vite keeps running, and the cookie stays valid. `--fresh`, a different mode (`BILLING`, `FAKE_SERVERS`, `FAKE_GITHUB`, `SERVERS`), a different port, new dependencies, an edited `up.sh` or a dead Hosted DNS restart vite and Postgres. It prints the URLs and the two `agent-browser` lines to run, and saves them to `.verify/run/info`.

The seed (`scripts/verify/seed.ts`) writes through the app's own Config Store commands:

| Where | What |
| --- | --- |
| organization `ada`, project `shop` | |
| `production` | services web, api, postgres (with volume pg-data), worker, redis; config sentry (`config.yml` referencing redis and the secret `api.SECRET_KEY`, and `sentry.conf.py`) mounted on web and worker at `/etc/sentry`; domain acme.com on web; one queued Deploy "Ship everything", whose nodes (its Services, pg-data and sentry) the seed writes into Applied State as if it had finished (without `SERVERS`), so they read as deployed; unpublished api edits (image whoami v1.11.0, `FEATURE_SEARCH`) |
| `fix-api` | a Branch of production holding api and web, with 2 changes to save |
| uptime (`scripts/verify/seed-uptime.ts`) | Monitoring tab states in `production`: web Up with 90 days and a closed incident, api Unreachable, worker Checking, redis Unreachable with a 404 at its healthcheck path `/health`, postgres Not checked. It seeds in the background, so `up.sh` returns before it ends. Before checking the Monitoring tab, wait for `.verify/run/uptime` to say `ok` (`until [ -s .verify/run/uptime ]; do sleep 1; done; cat .verify/run/uptime`); `failed` points at `.verify/run/seed-uptime.log` |
| metrics (`src/modules/metrics/metrics.verify-fixture.ts`, without `SERVERS`) | Services: web normal on Servers `0123…` and `1111…`; api mixed (`0123…` answers, `2222…` needs upgrade, `3333…` not up); worker normal on `0123…`, with `1111…` answering as a Server without it does (no points, one gap over the range); redis needs upgrade on `1111…` and `2222…`; postgres normal on `0123…` (`1111…` answers without it) with a gap in the middle third, an OOM then a restart, and disk reads and writes; any other Service no readings (both Servers answer without it). Servers: `0123456789abcdef0123456789abcdef` normal with CPU, memory, eth0 (lo and docker0 hidden) and every disk field, with a short not-sampled gap mid-range; `2222…` needs upgrade; `4444…` answers with no readings; `1111…` and any other id not up |

With `SERVERS`, the seed skips the fake Server and the queued Deploy, so it writes no Applied State, and `seed.json` lists them under `skipped`. To seed different state, add Config Store writes to `seed.ts`. It is typechecked with the app, so a domain refactor that breaks it fails `pnpm typecheck`.

## Doctor

```bash
scripts/verify/doctor.sh      # read-only; one line per check, exit 1 on any FAIL
```

It checks `node_modules`, the SDK, the container, vite, the uptime seed and a signed-in `/cloud` redirect. With `SERVERS` it also checks Inngest, the worker and `verify-cluster doctor`. A FAIL names its fix. Read `.verify/run/vite.log` for server errors.

## Drive

```bash
S=$(cat .verify/run/session)
# paste the two agent-browser lines from .verify/run/info (cookies set, then open), then:
agent-browser --session $S snapshot -i -c        # accessible names + refs
agent-browser --session $S click @e14
agent-browser --session $S screenshot "$PWD/.verify/evidence/<name>.png"
```

- Find controls by accessible name in `snapshot -i`, then `click @eN`. Refs change after each navigation, so snapshot again. `find role <role> --name` misses some links (the Branch link is one); refs always work. The handles for each area are in [features/](features/README.md).
- If a click reports that an element is "covered by" another, a sheet or dialog is open. Close it first, with `press Escape` or its **Close** button.
- Pass `screenshot` an **absolute** path. A relative path lands in agent-browser's temp dir.
- Single-quote URLs that contain `~` or `$` (`'/cloud/ada/~/servers'`).
- Inspector textboxes save on Enter, not on blur. A `fill` that you never commit is gone after a reload.
- After any write, `reload` and snapshot again. That proves the write persisted rather than living only in optimistic UI.

## Evidence

Save evidence to `dashboard/.verify/evidence/`, which is gitignored and survives `down.sh`. A verification counts as proof only when it:

1. drives the real user path (clicks and typing, not a URL or a database write standing in for them),
2. captures both the action and the resulting state (a screenshot or snapshot before and after),
3. shows that the effect persisted (reload; or query the database with `docker exec <session> psql -U postgres -d ployz_cloud`),
4. reports every check that could not run, along with the reason.

## Cleanup

```bash
scripts/verify/down.sh        # stops vite, Inngest, the worker and the cluster; removes the container and browser session; keeps .verify/evidence
```

The instance runs `down.sh` by itself after 15 minutes with nothing connected to vite; an open page counts as connected. `VERIFY_IDLE_TIMEOUT=<seconds>` changes that, and `0` keeps it up.

## Known gaps

- Without `SERVERS`, no Server answers. Service nodes read **Queued · Can't reach servers** and Logs and runtime status stay empty. Without `SERVERS` or `FAKE_GITHUB`, Inngest points at a dead port, so anything that sends an Inngest event fails at that call. With only `FAKE_GITHUB`, Inngest runs the seeded queued Deploy, which fails because no Server answers.
- Without `SERVERS`, metrics are fixture replies (`PLOYZ_VERIFY_METRICS=1`), not readings; they prove the charts and states render, not that a daemon samples.
- `FAKE_SERVERS=1` (`PLOYZ_VERIFY_RUNTIME=1`) fakes the Runtime Watch with four Servers: fra-1 (`0123…`) answers metrics, fra-2 (`1111…`) is offline and doesn't answer, syd-1 (`2222…`) runs 0.2.4 and needs an upgrade, ams-1 (`4444…`) answers with no readings yet. Only the Servers list and what reads the watch see them; deploys still go nowhere.
- Hosted DNS is fake (`scripts/verify/hosted-dns.ts`, requests in `.verify/run/hosted-dns.log`). It grants `<org slug>.ployz.test` and signs certificates with a throwaway CA, so generated addresses deploy but resolve nowhere and fail TLS verification.
- Polar is fake. Completing checkout cannot be verified here; `BILLING=1` only renders the billing UI.
- Without `FAKE_GITHUB`, GitHub is unreachable. `FAKE_GITHUB=1` starts `scripts/verify/fake-github.ts`, sets `PLOYZ_VERIFY_GITHUB_ORIGIN` so every GitHub API, web and codeload URL Cloud builds goes to it, including the Source, Add workflow and pull request checks links. It also starts Inngest and the worker so webhooks sync repositories. Cloud refuses that variable in production and logs the origin it uses at boot; `.verify/run/info` prints it as `github origin:`. The fake logs each request to `.verify/run/fake-github.log` and reads its fixture from `.verify/github.json`. Every start merges the current defaults into that file and writes it back: your values win, and default users, installations and repositories whose id the file lacks are added, so a deleted default comes back (set `"awaitsGrant": true` on a repository to hide it, `false` to grant it). `down.sh` keeps the file, so edits apply from the next `up.sh`. Users are ada (9001, Ada's GitHub account), grace (9002, linked to no Cloud user, so her installs reach no Organization) and bob (9003, Bob's GitHub account). Installations are 7 on acme (account 5001; ada, grace) with acme/blog (public), acme/web and acme/docs (not yet granted); 8 on ada with ada/notes; 9 on hopper-labs (account 5003; grace) with hopper-labs/compiler; and 10 on bob with bob/shop. A User installation's account id is its user's id, and webhook payloads carry it as `installation.account.id`. Under `FAKE_GITHUB` the seed links Ada's Organization to installations 7 and 8 and caches their granted repositories, as a sync leaves them, and creates Bob Bobson (bob@example.com, password `verify-bob-password`) with his own personal Organization, linked to installation 10 the same way. A link row is what grants an Organization an installation: the installation webhook records and syncs one but links no Organization, and `ployz github disconnect` drops a link. `/auth` has no email form, so `up.sh` signs Bob in through `POST /api/auth/sign-in/email` and prints two `agent-browser` lines for a second session, `<session>-bob`, that land him on `/cloud/bob`. To sign him in again by hand, for example after a reseed:

  ```bash
  base=http://localhost:<port>; S=$(cat .verify/run/session)-bob
  c=$(curl -s -o /dev/null -D - -X POST $base/api/auth/sign-in/email -H 'content-type: application/json' -H "origin: $base" \
    -d '{"email":"bob@example.com","password":"verify-bob-password"}' | tr -d '\r' | sed -n 's/^set-cookie: better-auth\.session_token=\([^;]*\).*/\1/Ip')
  agent-browser --session $S cookies set better-auth.session_token "$c" --url $base --httpOnly --sameSite Lax
  agent-browser --session $S open $base/cloud
  ```

  Every repository has branches main and dev holding a Dockerfile and index.html, and any 40-hex sha resolves to those files, so seeded commits download too. **Connect GitHub** opens the fake install page; picking an installation sends the signed `installation` webhook and closes the popup; Cloud records the installation and syncs its repositories, and only a link row grants them to an Organization. Set `app.afterInstall.callbackPath` (for example `/api/github/callback`) to redirect to that path on Cloud's origin instead, with `code`, `installation_id`, `setup_action` and `state`. Reload to see the synced repositories. The OAuth authorize page (`/login/oauth/authorize`, `?login=ada` skips the choice) redirects to `redirect_uri` with `code` and `state`, and `/login/oauth/access_token` swaps the code for a user token. Send any other webhook through the fake, signed with `verify-webhook-secret`: `POST /_fake/webhook` with `{"event":"installation_repositories","action":"added","installation":7,"repositories":["acme/docs"]}` (or `removed`, which drops the repository from every picker without a Refresh), or `{"event":"installation","action":"deleted","installation":7}`; `.verify/run/info` prints the curl line. An optional `sender` names the GitHub login the webhook comes from, and defaults to the installation's first user. Signing in with GitHub still goes to the real github.com.
  - `{"event":"installation","action":"suspend","installation":7}` suspends it: its token and its `access_tokens` call answer 403 and `GET /app/installations/7` carries `suspended_at`; `unsuspend` restores it.
  - `/_fake/install?installation_id=7&order=callback-first` redirects to the callback first and sends the `installation` webhook 700ms later, so Cloud sees the callback before the install.
  - `/_fake/install?installation_id=7&setup_action=update` redirects with `setup_action=update` and sends no webhook.
  - `{"event":"pull_request","action":"opened","installation":7,"repository":"acme/blog","number":5,"head_branch":"feature"}` creates or updates pull request 5, served at `/repos/acme/blog/pulls/5`; `synchronize` moves its head, `edited` with `"target_branch":"dev"` changes its base, `closed` takes `"merged":true`.
  - A repository's optional `pullRequests` fixture field seeds pull requests: `[{"number":3,"head_branch":"dev","title":"Seeded"}]`.
  - `{"event":"push","installation":7,"repository":"acme/blog","ref":"refs/heads/main"}` sends a push of that branch's head.
  - `{"event":"check_suite","action":"completed","installation":7,"repository":"acme/blog","conclusion":"failure"}` sends a GitHub Actions check suite on the default branch head (`"head_sha"` overrides it), served at `/repos/acme/blog/check-suites/<id>`.
  - `POST /_fake/oidc` with `{"aud":"<audience>","repository_id":4242,"run_id":30001,"job_workflow_ref":"acme/blog/.github/workflows/ployz-build.yml@refs/heads/main"}` returns `{"token":"<RS256 JWT>"}` signed by the key at `/.well-known/jwks`, issued by the fake's origin.
  - The build workflow exists only where a repository's fixture `files` hold `.github/workflows/ployz-build.yml`; dispatches start runs at 30001 that stay `in_progress` until `POST /_fake/run` with `{"repository":"acme/blog","run_id":30001,"status":"completed"}`.

## Behavior reference

`docs/user/` describes shipped behavior from the user's point of view. Check a result against it before calling it right or wrong.
