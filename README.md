# ployz2

Ployz Cloud, CLI, and daemon for a cluster of Docker machines

## Install

```sh
curl -fsSL https://ployz.sh | sh
brew install getployz/ployz/ployz
```

Release process: [docs/RELEASE.md](docs/RELEASE.md).

## Workspace

- `crates/ployz-core`: domain and wire contracts shared by both binaries
- `crates/ployz`: CLI for Linux, macOS, and Windows through WSL
- `crates/ployz-relay`: Cloud Relay HTTP/1.1 WebSocket splice (Linux binary + `ghcr.io/getployz/ployz-relay`)
- `crates/ployz-sdk`: napi package `@ployz/sdk` (linux and macOS x64/arm64 gnu bindings; published on GitHub Release Publish). Its TypeScript declarations are derived from the Rust wire types by `cargo test -p ployz --test sdk_payloads`
- `crates/ployzd`: Linux-only daemon
- `crates/ployz-testkit`: unpublished support crate used only by tests

Building `ployz` also requires Go 1.24 or newer. Cargo builds and embeds the Compose helper; installed users need neither Go nor the Docker Compose plugin. Local builds require Docker with Buildx and the containerd image store.

`ployz build` and `ployz deploy` prefer a declared or existing default Dockerfile.
Buildable Services without one use Railpack; image-only Services are unchanged.
Set `build.x-recipe` to `dockerfile`, `railpack`, or `auto` (the default) to control
selection. A failed recipe never falls back to another.

Railpack uses matching pinned 0.39.0 preparation/frontend tooling with BuildKit
0.26.2, provisioned through Docker, and builds one native Linux AMD64 or ARM64
image. Service variables default build variables; Compose `build.args` and then
`--build-arg` override them without changing runtime values. Values travel as
private secret mounts. Docker ignore patterns and Railpack's configured
exclusions apply before source transfer. Recipes can still print or embed values.

Railpack refuses `--check` and unsupported frontend settings by name. On this
pinned frontend, `--no-cache` and `--pull` force a cold build by clearing the
exclusive Ployz builder cache; unrelated Docker builder caches are untouched.

Run the fast local gate with `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets --all-features -- -D warnings`, and `cargo test --workspace --all-features`.

Cloud lives in `cloud/` with its own package and lockfile. Run `pnpm install --frozen-lockfile` and `pnpm pr:check` there. Engine Cargo commands run from the repository root. `site/` serves the ployz.sh installer CDN.

Cloud installs the published `@ployz/sdk`, including platform bindings, pinned to the workspace version. Run `python3 scripts/check-cloud-sdk-version.py` to check those pins. See [the repository layout decision](docs/adr/0004-product-repo-layout.md).

Production cutover uses the existing Railway **Ployz Dashboard / production / web** service: change its source to `getployz/ployz2`, branch `main`, and root directory `/cloud`. Keep its existing variables, domains, and `pnpm db:migrate` pre-deploy command. This repository change does not apply those hosted settings.

Cloud migrations now start from one fresh baseline for the planned Railway reset. Apply it only to an empty Cloud database with a fresh Drizzle migration journal. Reset Electric's persisted sync state together with Postgres before cutover, then run `pnpm db:migrate` from `cloud/`. Historical migrations remain in the original dashboard repository history.
