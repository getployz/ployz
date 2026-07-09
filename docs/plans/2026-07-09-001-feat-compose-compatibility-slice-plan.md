# Compose Compatibility Slice: Parser + Diagnostics + Easy Runtime Fields

## Context

Ployz's Compose parser accepts only `name`/`image`/`deploy.replicas`/`x-route` and `deny_unknown_fields` rejects every real Compose file — the #1 blocker in `~/dev/ployz-planning/docs/uncloud-ployz-feature-gap.md` and P2 of the parity roadmap. This slice makes `ployzctl deploy -f` accept real Uncloud-shaped Compose files: full parse with exhaustive diagnostics, plus four container fields threaded end-to-end to Docker. Greenfield — no deployments exist; wire types, digests, and evidence shapes change in place with no compat shims. Long-term shapes everywhere.

**Decided scope**: (1) parser + diagnostics for everything, with `command`/`entrypoint`/`environment`/`stop_grace_period` deployed for real; healthcheck/volumes/ports/global-mode/update-order/x-pre_deploy parsed + typed but diagnose-only. (2) `${VAR}` interpolation + `.env` + `env_file`; profiles deferred. (3) Strict by default — unsupported field = hard error listing ALL findings; `--allow-unsupported` downgrades to warnings and deploys the supported subset. No silent ignores (Uncloud silently ignores `profiles`/`restart`/`deploy.placement`; we classify everything).

Verified enablers: bollard's `ContainerCreateBody` already has `env`/`cmd`/`entrypoint`/`stop_timeout` (bollard-stubs models.rs:1211,1326); `stop_managed_container` (docker/runner.rs:259) builds stop options without `t`, so create-time `StopTimeout` governs stops for free (ADR 0008's bounded grace).

## Architecture decisions

- **D1 — Interpolate the parsed `Value` tree** (`serde_yaml::from_str::<Value>` → `apply_merge()` → interpolate string scalars → deserialize typed structs). Raw-string interpolation corrupts YAML; post-typed can't handle `replicas: ${R:-2}`. Grammar: `$VAR`, `${VAR}`, `${VAR:-d}`, `${VAR-d}`, `${VAR:?e}`, `${VAR?e}`, `$$`. Unset-no-default → empty string + advisory warning. Interpolation env is an explicit `&BTreeMap<String,String>` param (test seam) — production = `.env` beside compose file overlaid by OS env (OS wins). `.env` feeds interpolation only, never containers.
- **D2 — Diagnostics via typed structs + exhaustive destructuring.** Drop `deny_unknown_fields`. Every struct: real typed fields for supported AND diagnose-only features, plus `#[serde(flatten)] unrecognized: BTreeMap<String, Value>`. Classifier destructures exhaustively (new field = compile error = forced classification). Unrecognized keys route through one `classify_service_key(&str) -> Option<KnownUnsupported>` match (~40 arms). Per-service error accumulation: parse `services` as `BTreeMap<String, Value>`, deserialize each independently so ALL findings are collected, not serde-fail-fast.
- **D3 — One `ContainerRuntimeSpec` struct** carries command/entrypoint/environment/stop_grace_period through all five hops (DeployServiceSpec → DeployServiceRequest → MachineContainerRunRpcRequest → CreateManagedContainer → create_body). Future fields (healthcheck, user, init) join this struct.
- **D4 — Digests move to length-prefixed framing** (`tag, u64-be len, bytes`): the current `\nfield=` encoding is injectable once arbitrary env values enter the hash. Constants bump: `ployz.namespace_revision_entry.v3` (adds the four fields — all force container replacement), `ployz.namespace_revision.v2`. Container reuse keys on entry id via labels, so changed env ⇒ new entry id ⇒ replace automatically. `ManagedContainerIdentity`/labels stay spec-free (ADR 0022).
- **D5 — Env is plaintext on the wire and in evidence** (DeploySubmitted stores the full DeployRequest; sdk-types is TS-exported). This is deploy intent, not runtime observation — document in compose-support.md ("environment is for non-sensitive config; secrets are the planned mechanism"), implement nothing.

## New core types (ployz-core/src/deploy.rs)

```rust
pub struct EnvName(String);   // non-empty, no '=', no NUL
pub struct EnvValue(String);  // NUL-free
pub struct ServiceEnvironment(BTreeMap<EnvName, EnvValue>);  // deterministic for digests
pub struct ContainerCommand(Vec<String>);  // exec-form argv, non-empty (shell-form split client-side via shell-words)
pub enum ContainerEntrypoint { Clear, Argv(Vec<String>) }  // Clear = compose `entrypoint: []`/"" → Docker Some(vec![])
pub struct StopGracePeriod(u32);  // whole secs; DEFAULT = 10; parser resolves default so wire always carries it
pub struct ContainerRuntimeSpec {
    pub command: Option<ContainerCommand>,
    pub entrypoint: Option<ContainerEntrypoint>,
    pub environment: ServiceEnvironment,
    pub stop_grace_period: StopGracePeriod,
}
// + ContainerRuntimeSpec::image_defaults() for flag-driven deploys/tests
// DeployServiceSpec + DeployServiceRequest gain `runtime: ContainerRuntimeSpec` (required, no serde default)
```

## New parser module (ployzctl/src/compose/ replaces compose.rs)

```
mod.rs         load → merge-keys → interpolate → parse → classify → decide
interpolate.rs ${VAR} grammar over serde_yaml::Value (pure)
env_files.rs   dotenv parse + per-service env_file merge (pure)
model.rs       typed compose document, no deny_unknown_fields, flatten-unrecognized
diagnostics.rs ComposePath (dotted), ComposeFinding { path, kind },
               ComposeFindingKind { InvalidValue{message} (always fatal),
                 Unsupported{feature: KnownUnsupported}, UnknownField,
                 Advisory{message} (never fatal) },
               KnownUnsupported enum (~25 variants, each with status() + guidance()),
               UnsupportedFieldMode { Strict, AllowUnsupported },
               ComposeDiagnostics::resolve(mode) -> Result<Vec<RenderedWarning>, ComposeRejection>
translate.rs   classify_service: exhaustive destructure of ComposeService → (findings, Option<DeployServiceSpec>)
```

Entry: `parse_deploy_file(ComposeInput { source, base_dir, interpolation_env, namespace_override, mode }) -> Result<(ParsedComposeDeploy, Vec<RenderedWarning>), PloyzctlCliError>`.

Env merge order (documented contract): `env_file` in listed order (later wins) → `environment` overrides. Map-form null value (`FOO:`) resolves from CLI process env or omits with Advisory. `env_file` paths relative to compose dir; missing = InvalidValue unless `required: false`. Rendering: one line per finding, sorted by path: `services.web.healthcheck  unsupported (planned)  <guidance>; remove it or pass --allow-unsupported`.

New dependency: `shell-words` (shell-form command splitting). Everything else stdlib + existing deps.

## Phases (each leaves the tree green)

1. **Core types + digest v3/v2** (ployz-core): types above; `runtime` on both spec/request; digest rewrite with framing + constant bumps; update every `DeployServiceSpec {` construction site (ployzctl, ployzd tests, ployz-e2e, sdk-types typescript.rs:459) via `image_defaults()`; regenerate TS + contract test. Tests: per-field digest change, env-order stability, framing collision (env value containing `\nimage=`).
2. **Runtime threading** (ployzd): `runtime` on `MachineContainerRunRpcRequest` (roles/machine/protocol.rs), `CreateManagedContainer` (roles/machine/runner.rs), containers.rs handler, operations/deploy/mod.rs:757 pass-through; `create_body` (adapters/docker/runner.rs:502) sets env (K=V vec), cmd, entrypoint (Clear→`vec![]`), `stop_timeout: Some(grace as i64)`. Extend create_body unit tests (~619) + machine RPC test builders.
3. **Preprocessing** (compose/interpolate.rs, env_files.rs): pure modules, full grammar unit tests, not yet wired.
4. **Parser rewrite + taxonomy**: model/diagnostics/translate/mod as sketched; `--allow-unsupported` flag on DeployCli (rejected without `-f`); `PloyzctlCliError::ComposeRejected { rendered }`; warnings to stderr before submit; update deploy_cli_contract.rs.
5. **Fixture harness**: `tests/compose_fixtures.rs` walks `tests/fixtures/compose/<case>/`. File-set encodes outcome: `compose.yaml` + optional `case.env` (fake OS env — never real process env) / `dotenv` / `*.env` / `allow_unsupported` marker; `expected.request.json` (byte-exact DeployRequest) and/or `expected.diagnostics` (rendered text). `UPDATE_FIXTURES=1` bless mode. ~16 cases incl. `ok_full_runtime`, `ok_env_merge_order`, `ok_interpolation`, `ok_entrypoint_clear`, `err_strict_kitchen_sink` (all diagnose-only features + unknown key in one file — pins all-findings listing), `warn_allow_unsupported_kitchen_sink`, `err_service_value_errors_accumulate`.
6. **E2E + docs**: dind e2e deploy with environment+command (busybox `sh -c 'env; sleep 600'`), assert `Config.Env`/`Config.Cmd` via container inspect; compose-support.md classification table + merge-order + interpolation grammar + plaintext-env note; short ADR: "strict by default; --allow-unsupported downgrades Unsupported/Unknown, never InvalidValue".

## Critical files

- `crates/ployz-core/src/deploy.rs` — types + digests
- `crates/ployzctl/src/compose.rs` → `crates/ployzctl/src/compose/` module tree
- `crates/ployzctl/src/commands/deploy.rs` — flag, ComposeInput wiring, route shorthand reuse (parse_route_shorthand:244)
- `crates/ployzd/src/roles/machine/{protocol,runner,containers}.rs` — RPC hop
- `crates/ployzd/src/operations/deploy/mod.rs` — step pass-through
- `crates/ployzd/src/adapters/docker/runner.rs` — create_body
- `crates/ployz-sdk-types` + `packages/ployz-sdk/src/generated.ts` — TS regen

## Verification

- Per phase: `cargo test -p <crate>`, clippy. Final: `cargo fmt --check`, `cargo clippy --workspace --all-targets`, `cargo test --workspace`, TS generated-contract test, fixture suite.
- End-to-end: dind e2e (`PLOYZ_DIND_E2E=1 scripts/dind-e2e.sh`) with the env+command deploy assertion.
- Manual smoke: `ployzctl deploy -f` with kitchen-sink fixture in strict mode (see all findings rendered) and with `--allow-unsupported` (warns + deploys subset).

## Risks

- Constructor churn across ~10 test files (mechanical; compiler enumerates sites).
- serde_yaml 0.9 unmaintained — isolated to the compose module; swap later, not this slice.
- Guidance-text goldens are brittle by design (user-facing contract); bless mode keeps updates cheap.
- Old local evidence dirs in dev environments won't replay DeploySubmitted (greenfield, accepted).

## Non-goals

Executing healthchecks/volumes/ports/global-mode/update-order/x-pre_deploy (diagnose-only); profiles resolution; secrets/configs; pull policy/registry digests; env redaction; depends_on phases; multi-file/extends/overrides; COMPOSE_PROJECT_NAME; Uncloud's nanocpus-fraction quirk; wire compat.
