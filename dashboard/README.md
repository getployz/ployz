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
```

Configure `.env` before running migrations or starting the app. `pnpm dev` builds
the SDK and starts the app through Portless. `pnpm pr:check` runs the local CI gate.

Compose uses the stable project name `ployz-cloud` so moving the checkout keeps
the same database volume. If an existing local stack uses another project name,
retain it with `docker compose -p <existing-name>`.

## Build and deploy

`pnpm build` compiles the native SDK and browser WASM from
`../core/crates/ployz-sdk`, builds the application, and packages the native runtime
into `.output/`. `pnpm start` starts that output.

Hosting must include both `dashboard/` and `core/` in the checkout, with commands
running from `/dashboard` and Rust/Go available during the build. When this layout
lands, update the existing Railway **Ployz Dashboard / production / web** service's
root directory to `/dashboard`. Retain its variables, domains, and
`pnpm db:migrate` pre-deploy command. This repository change does not apply hosted
settings.

See [DESIGN.md](DESIGN.md) for product design and [CONTEXT.md](CONTEXT.md) for the
Dashboard glossary.
