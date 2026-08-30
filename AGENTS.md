# Change workflow

Use `$implement` for Rust changes.

For `$implement`, `$four-axis-review` supersedes `$code-review`. After implementation, follow its incremental rerun loop until all four axes pass. Do not run `$four-axis-review` on docs, CI, research, or scripts.

Prefer simple diagrams. Use `$i-have-adhd` output.

## Testing rungs

After a behavior change, name the **rung** and the test. Climb only when a lower rung cannot go red for the bug. Look up the path in `evidence/product-paths.tsv`. Fill a `gap` at the lowest empty honest rung.

1. Fastest local check (crate unit, or a Fast CI shell contract such as `scripts/test-cli-installer.sh`)
2. Layer 1 semantic (`cargo test`, not ignored)
3. CLI shape (`ployz/tests/cli_shape.rs`, `*_cli.rs`)
4. Informing cluster (`#[ignore = "informing"]` and listed in `scripts/run-layer3-tests.sh`)
5. Authority (`scripts/qualify-release.sh` against musl archives on real Machines)

Never add `#[ignore = "informing"]` unless that test binary is in `scripts/run-layer3-tests.sh`.
