# Product repo is `crates/`, `cloud/`, and `site/`

When Ployz Cloud joins this repository, the git root stays a Cargo virtual workspace. Crates move under `crates/` with directory name equal to crate name. Cloud application code lands as `cloud/` (package `ployz-cloud`). `site/` stays the ployz.sh installer CDN. The `ployz` crate is not renamed.

```
Cargo.toml                 # members = crates/*
crates/ployz/              # CLI; compose-helper stays inside (Go, not a crate)
crates/ployz-core/
crates/ployzd/
crates/ployz-relay/
crates/ployz-sdk/          # napi @ployz/sdk
crates/ployz-testkit/
cloud/                     # today's dashboard tree, including src/routes/_public
site/                      # ployz.sh (_headers), not marketing
```

`cargo test --workspace` keeps running from the git root. Cloud keeps its own `package.json`, lockfile, and `pnpm pr:check` with working directory `cloud/`. Hosted build root is `cloud`. Production currently uses Railway; the earlier plan named Vercel. There is no root `package.json` and no `apps/` or `packages/` tree.

Engine `DESIGN.md` and `CONTEXT.md` stay at the git root (Machine, Cluster, Deploy). Cloud keeps `cloud/DESIGN.md` and `cloud/CONTEXT.md` (Server, Saved State). Those files are not merged.

`@ployz/sdk` stays a published napi package. `cloud/package.json` pins it (and the platform optionalDependencies) to `[workspace.package].version`. `scripts/pack-sdk-package.sh` injects those optionalDependencies at pack time; the source `package.json` is not the install shape the hosted build needs, and the hosted Cloud build does not compile Rust. Local iteration may `file:` a packed tarball; that override is not checked in. CI fails a PR where the Cloud pin disagrees with the workspace version.

Marketing home, pricing, and docs already live in Cloud `_public` routes. They stay there. A posthog.com-style website repo is only for a handbook or blog that outgrows the signed-in app.

Rejected: crate directories dumped at git root; a nested `rust/Cargo.toml` (PostHog does that because Django owns `/`; this repo's release path is already Cargo); naming the crate folder `engine/`; OpenShip/Supabase `apps/*`+`packages/*`; extracting `packages/ui` or a shared database package; `workspace:*` as Cloud's production install; compiling the cdylib on Vercel; renaming crate `ployz`.

Cloud application code, migrations, and app-specific `AGENTS.md`, `DESIGN.md`, and `CONTEXT.md` are imported. Cloud's internal docs, ADRs, and bundled agent skills are excluded.
Cloud's migration history is replaced with one initial migration and snapshot for the planned database reset; no in-place transition from the old migration journal is provided. Electric replica identity is retained, and unused LISTEN/NOTIFY triggers are omitted.
