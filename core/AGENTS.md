# Core

Run project commands from `core/`.

## Testing rungs

After a behavior change, name the **rung** and the test. Climb only when a lower rung cannot go red for the bug.

1. Fastest local check (crate unit, or a Fast CI shell contract such as `scripts/test-cli-installer.sh`)
2. Layer 1 semantic (`cargo test`, not ignored)
3. CLI shape (`crates/ployz/tests/cli_shape.rs`, `*_cli.rs`)
4. Informing cluster (`#[ignore = "informing"]` and listed in `scripts/run-layer3-tests.sh`)

Never add `#[ignore = "informing"]` unless that test binary is in `scripts/run-layer3-tests.sh`.

## Recipes

- SDK payload types stale: `pnpm sdk` in `dashboard/` builds or fetches `crates/ployz-sdk/generated/` with the binding it matches. Nothing there is committed.
- Config Store suite on Postgres (CI runs it; without the variable the suites run on SQLite):
  `docker run -d --rm --name store-pg -p 127.0.0.1:5433:5432 -e POSTGRES_PASSWORD=postgres postgres:16-alpine` then
  `PLOYZ_STORE_TEST_POSTGRES=postgres://postgres:postgres@127.0.0.1:5433 cargo nextest run -p ployz-store`.
- Dashboard's native SDK after a Rust change: `scripts/build-cloud-sdk.sh`.
