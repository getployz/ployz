# Ployz Dashboard

Ployz Cloud: web UI, backend, and durable workflows. It authors each Deploy
Intent and drives Clusters through Machine RPC; it is not runtime authority.
The deployment engine and SDK live in [core/](../core/README.md).

## Develop

Run commands from `dashboard/`. Install Node and pnpm versions from `package.json`,
plus Rust (rustup) for the locally linked SDK (see [core](../core/README.md)).

```sh
pnpm install --frozen-lockfile
cp .env.example .env
docker compose up -d
pnpm db:migrate
pnpm dev
# In a second terminal (rebuild after worker code changes):
pnpm exec vite build --config vite.worker.config.ts
PORT=3001 INNGEST_DEV=1 pnpm start:worker
```

Configure `.env` before running migrations or starting the app. `pnpm dev` builds
the SDK and starts the app through Portless. `pnpm pr:check` runs the local CI gate.

Compose uses the stable project name `ployz-cloud` so moving the checkout keeps
the same database volume. If an existing local stack uses another project name,
retain it with `docker compose -p <existing-name>`.

## Environment

Web and worker read the same variables and exit with a `ConfigError` naming
what is missing or invalid.

| Variable | Required | Notes |
| --- | --- | --- |
| `DATABASE_URL` | yes | Postgres URL |
| `APP_URL` | yes | public URL of web |
| `BETTER_AUTH_SECRET` | yes | auth session secret |
| `APP_ENCRYPTION_SECRET` | yes | 32+ characters; encrypts stored credentials, keep it stable |
| `GITHUB_CLIENT_ID`, `GITHUB_CLIENT_SECRET` | yes | GitHub OAuth App (sign-in) |
| `GITHUB_APP_ID`, `GITHUB_APP_SLUG`, `GITHUB_APP_PRIVATE_KEY`, `GITHUB_APP_WEBHOOK_SECRET` | yes | GitHub App (repository access) |
| `INNGEST_EVENT_KEY`, `INNGEST_SIGNING_KEY` | yes | shared with the Inngest server |
| `INNGEST_BASE_URL`, `INNGEST_CONNECT_GATEWAY_URL` | no | Inngest API and Connect gateway; local dev defaults otherwise |
| `BETTER_AUTH_TRUSTED_ORIGINS`, `PORT` | no | `PORT` defaults to 3000 |
| `POLAR_ACCESS_TOKEN`, `POLAR_WEBHOOK_SECRET`, `POLAR_PRODUCT_ID`, `POLAR_SERVER` | no | all or none; none disables billing and every Organization is unlimited |

## Build and deploy

`pnpm build` compiles the native SDK and config WASM, then builds the web and
Connect worker into `.output/`. `Dockerfile.cloud` packages both in one image:

| Process | Start command | Healthcheck |
| --- | --- | --- |
| Web | `npm start` | `/` |
| Worker | `npm run start:worker` | `/ready` |

Run one worker alongside the web service. Both use the same application variables
and database; only web needs public ingress. The worker uses `INNGEST_BASE_URL`
for the Inngest API and, for a self-hosted gateway, set
`INNGEST_CONNECT_GATEWAY_URL=ws://inngest.railway.internal:8289/v0/connect`.
The equivalent local dev defaults are ports 8288 and 8289. Use different `PORT`
values when running both processes directly on the same host.

Connect registers the `ployz-cloud` functions automatically; web exposes no
`/api/inngest` route. Both processes run the same image.

On SIGTERM/SIGINT, Connect stops accepting new steps and finishes active steps
before the worker disposes database and SDK resources. Set Railway worker
`RAILWAY_DEPLOYMENT_DRAINING_SECONDS=1800` (or a longer operational allowance);
on Docker use the corresponding `stop_grace_period`. This is a shutdown grace
period, not a deployment execution timeout. A crash or forced kill can still
leave remote effects unknown; interrupted deployment steps are not blindly retried.

Run migrations once before a rollout, not independently on each process.

See [DESIGN.md](DESIGN.md) for product design and [CONTEXT.md](CONTEXT.md) for the
Dashboard glossary.

### Cloud image deployment

The existing Cloud workflow reuses the matching SDK artifact (and the shared
kache/R2 compiler cache on misses), builds both entrypoints, and packages
`Dockerfile.cloud`. The final image smoke test loads the native SDK inside the
runtime container. CI uses Ubuntu 24.04; the runtime uses Node 24 on Debian Trixie
so its glibc supports that native artifact.

On every main push, the build job publishes `ghcr.io/getployz/ployz2-cloud:main`
and triggers Railway immediately, without waiting for the parallel checks.
New pushes cancel superseded runs. Publication and deployment also check that the
commit is still main. A deployment already accepted by Railway is not cancelled
by cancelling GitHub Actions.

One-time setup:
- Set the repository secret `RAILWAY_TOKEN` to a Railway project token scoped to
  Ployz Dashboard's production environment.
- Make the GHCR package public after its first publication (or configure Railway
  with registry read credentials).
- Set web and worker to that Docker image source instead of the GitHub repository.
  Keep web's migrations/start command, and worker's start override, `/ready`,
  and 30-minute drain. The service IDs live in `scripts/publish-cloud-image.sh`.
- Disable scheduled image auto updates: CI runs `railway redeploy --from-source`
  for both services after the image push.

Each release tag also publishes `ghcr.io/getployz/ployz-cloud:<tag>` from the
same build, and attaches `ployz-cloud-compose.yml` (pinned to that tag) and
`ployz-cloud.env.example` to the GitHub release.

## Self-host

Self-hosted Cloud runs the released image as web and worker beside Inngest,
Redis, and Postgres ([`self-host/compose.yml`](self-host/compose.yml)). You bring
your own GitHub apps; billing is off, so Organizations are unlimited. The relay,
Hosted DNS, installer, and release binaries remain Ployz-hosted.

```
browser ──TLS proxy──► web :3000 ──┐
GitHub webhooks ───────► web       ├─► Postgres (ployz_cloud, inngest)
                        worker ◄───┤─► Inngest :8288 API / :8289 Connect ─► Redis
```

The image is `linux/amd64`. Put a TLS proxy in front of web's `WEB_PORT`; Inngest's
UI is bound to `127.0.0.1:8288` for operator use only.

### 1. Create the GitHub apps

Replace `https://cloud.example.com` with your `APP_URL`.

**OAuth App** (sign-in) at GitHub → Settings → Developer settings → OAuth Apps:

| Field | Value |
| --- | --- |
| Homepage URL | `https://cloud.example.com` |
| Authorization callback URL | `https://cloud.example.com/api/auth/callback/github` |

Copy the Client ID to `GITHUB_CLIENT_ID` and a new client secret to `GITHUB_CLIENT_SECRET`.

**GitHub App** (repository access) at GitHub → Settings → Developer settings → GitHub Apps
(or under your organization):

| Field | Value |
| --- | --- |
| Homepage URL | `https://cloud.example.com` |
| Webhook URL | `https://cloud.example.com/api/github/webhook` (Active) |
| Webhook secret | `openssl rand -hex 32`, also `GITHUB_APP_WEBHOOK_SECRET` |
| Repository permissions | Contents: Read-only, Checks: Read-only, Metadata: Read-only |
| Subscribe to events | Push, Check suite |

Installation events are delivered without subscribing. Copy the App ID to
`GITHUB_APP_ID`, the URL name from `github.com/apps/<slug>` to `GITHUB_APP_SLUG`,
and a generated private key (the whole PEM, quoted) to `GITHUB_APP_PRIVATE_KEY`.

### 2. Configure and start

Download `ployz-cloud-compose.yml` as `compose.yml` and `ployz-cloud.env.example`
as `.env` from the release into one directory, then fill in `.env`. Every
uncommented variable is required; web and worker exit with a
`ConfigError` if one is missing.
Leave all `POLAR_*` variables unset.

```sh
docker compose run --rm web npm run db:migrate   # explicit, once per install/upgrade
docker compose up -d
docker compose ps                                # worker turns healthy once Connected
```

Neither web nor worker migrates on boot. The worker's healthcheck is `/ready`:
200 while its Inngest Connect connection is active, 503 otherwise. It is not a
database, schema, or GitHub check. Compose gives the worker a 30-minute
`stop_grace_period` so active steps drain.

### 3. Sign in, then install the GitHub App

Sign in to Cloud with GitHub first, then install the GitHub App as that same
GitHub user (from Cloud or `github.com/apps/<slug>`). Installation webhooks are
only attached to a GitHub account already linked in Cloud.

### Upgrade

Replace `compose.yml` with the new release's asset (or bump
`PLOYZ_CLOUD_VERSION`), then:

```sh
docker compose pull
docker compose run --rm web npm run db:migrate
docker compose up -d
```
