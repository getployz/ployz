---
name: verify-dashboard
description: Run a seeded, signed-in Ployz Cloud dashboard for this checkout and prove a UI change works in a real browser. Use after changing dashboard/ UI, routes, loaders or Config Store writes, when asked to verify, screenshot or click through Ployz Cloud, or when a fresh worktree needs the dashboard running.
---

# Verify the dashboard

One command gives this checkout its own Postgres, seed and `vite dev`, signed in as Ada Lovelace. Checkouts run side by side. Drive it with `agent-browser`. Run commands from `dashboard/`.

## Launch

```bash
scripts/verify/up.sh          # free port; BILLING=1 scripts/verify/up.sh turns billing on
SERVERS=2 scripts/verify/up.sh   # also real Servers paired through Cloud; DAEMON=stable (default), beta, checkout or a version
```

With `SERVERS`, `up.sh` also starts an Inngest dev server and the worker for this checkout, then runs `../core/scripts/verify-cluster` (see [verify-server](../verify-server/SKILL.md)). That command enrolls the Machines through this dashboard with an Organization Token. Deploys and enrollment follow-ups then really run. `.verify/run/info` adds the Inngest UI URL for function runs and the `verify-cluster cli` line for the cluster. Enrolling through Cloud needs the daemon version to equal this checkout's CLI version, so use `stable` on a release commit and `checkout` otherwise. The first run builds the shared VM image, which takes a few minutes.

A fresh worktree is fine: `up.sh` installs `node_modules` and builds the native SDK when either is missing or stale. The SDK build is shared across worktrees through `~/.cache/ployz/sdk`, keyed by the committed Rust sources, so only the first checkout of a given commit compiles it (about 2 minutes). A warm run takes about 20 seconds. Rerunning it restarts from a fresh seed, and the cookie stays valid. It prints the URLs and the two `agent-browser` lines to run, and saves them to `.verify/run/info`.

The seed (`scripts/verify/seed.ts`) writes through the app's own Config Store commands:

| Where | What |
| --- | --- |
| organization `ada`, project `shop` | |
| `production` | services web, api, postgres (with volume pg-data), worker; domain acme.com on web; one queued Deploy "Ship everything"; unpublished api edits (image whoami v1.11.0, `FEATURE_SEARCH`) |
| `fix-api` | a Branch of production holding api and web, with 2 changes to save |

With `SERVERS`, the seed skips the fake Server and the queued Deploy, and `seed.json` lists them under `skipped`. To seed different state, add Config Store writes to `seed.ts`. It is typechecked with the app, so a domain refactor that breaks it fails `pnpm typecheck`.

## Doctor

```bash
scripts/verify/doctor.sh      # read-only; one line per check, exit 1 on any FAIL
```

It checks `node_modules`, the SDK, the container, vite and a signed-in `/cloud` redirect. With `SERVERS` it also checks Inngest, the worker and `verify-cluster doctor`. A FAIL names its fix. Read `.verify/run/vite.log` for server errors.

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

## Known gaps

- Without `SERVERS`, no Server answers. Service nodes read **Queued · Can't reach servers**, Logs and runtime status stay empty, and Inngest points at a dead port, so anything that sends an Inngest event fails at that call.
- Hosted DNS always points at a dead port. Creating a hosted domain fails at that call.
- GitHub and Polar are fake. Connecting a repository or completing checkout cannot be verified here; `BILLING=1` only renders the billing UI.

## Behavior reference

`docs/user/` describes shipped behavior from the user's point of view. Check a result against it before calling it right or wrong.
