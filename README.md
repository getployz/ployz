# Ployz

Two projects in one repository:

| Project | Owns | Start here |
| --- | --- | --- |
| **Core** | Deployment engine, CLI, daemon, Rust SDK/WASM, native helpers, installer and releases | [core/README.md](core/README.md) |
| **Dashboard** | Hosted backend, web UI, durable workflows and marketing | [dashboard/README.md](dashboard/README.md) |

```text
core/         Cargo workspace and engine tooling
dashboard/    pnpm application
scripts/      Cross-project CI and SDK checks
.github/      Shared workflows
docs/         Shared domain guidance and decisions
```

Run Cargo commands from `core/` and pnpm commands from `dashboard/`.
Dashboard links `@ployz/sdk` directly from `core/crates/ployz-sdk`; its dev and
build commands compile that SDK. Changes across the boundary land together.

See the [context map](CONTEXT-MAP.md) for ownership and terminology.
