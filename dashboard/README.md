# Ployz Dashboard

The hosted Ployz application: web UI, backend, durable workflows, and marketing.
The deployment engine and SDK live in [core/](../core/README.md).

## Develop

Run commands from `dashboard/`. Install Node and pnpm versions from `package.json`,
plus Rust and Go for the locally linked SDK (see [core](../core/README.md)).

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

## Build and deploy

`pnpm build` compiles the native SDK and config WASM, then builds the web and
Connect worker into `.output/`. The root Dockerfile packages both in one image:

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

Connect registers the existing `ployz-cloud` functions automatically. Web no
longer exposes `/api/inngest` or performs HTTP function sync. Deploy worker code
independently of web-only changes. Publish the image once and point both services
at that image; do not rebuild the Railway-specific Dockerfile under another
service ID (its cache IDs belong to web).

On SIGTERM/SIGINT, Connect stops accepting new steps and finishes active steps
before the worker disposes database and SDK resources. Set Railway worker
`RAILWAY_DEPLOYMENT_DRAINING_SECONDS=1800` (or a longer operational allowance);
on Docker use the corresponding `stop_grace_period`. This is a shutdown grace
period, not a deployment execution timeout. A crash or forced kill can still
leave remote effects unknown; interrupted deployment steps are not blindly retried.

For the initial HTTP-to-Connect cutover, pause new deployment admission operationally,
wait for active workflows to settle, and remove the old HTTP app registration in
Inngest before starting the Connect worker. Replace web with this version so no
old replica can re-sync the HTTP registration. Keep existing Inngest storage and
the `ployz-cloud` app/function IDs. Verify only the Connect registration is active
before allowing new deployments. Do not run HTTP and Connect registrations side
by side. Run migrations once before the rollout, not independently on each process.

See [DESIGN.md](DESIGN.md) for product design and [CONTEXT.md](CONTEXT.md) for the
Dashboard glossary.

Variable Group authoring is disabled by default. Set `VITE_VARIABLE_GROUPS_ENABLED=true` before starting the dev server or building Dashboard to enable it. The flag is shared by UI and server authoring actions; existing attached values still participate in deployment when disabled.

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

The root `Dockerfile` remains the source-build path for self-hosting.
