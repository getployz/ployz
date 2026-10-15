# Workspace

- `core/` owns the engine, CLI, daemon, SDK, native helpers, and releases. Run Cargo there; apply relevant `core/CODING_STANDARDS.md` rules when changing core code.
- `dashboard/` owns Ployz Cloud: web, worker, durable workflows, and self-host packaging. Run pnpm there.
- Before designing a feature, read the affected project's `DESIGN.md`. Name each bet your change touches in the PR body, and the choice it changed. If you cross a red flag, argue it there. Edit a bet only when the bet itself changes.
- `docs/user/` describes shipped behavior from the user's point of view. Check it before deciding how something works today, and update it when user-visible behavior changes.
- Prove changes on the real artifact: dashboard UI with the `verify-dashboard` skill (add `SERVERS=1` for runtime UI), and anything needing real Machines (daemon, Docker, WireGuard, ZFS, moves) with the `verify-server` skill, before reaching for cloud VMs.
- When changing domain behavior, terminology, or ownership boundaries, follow [docs/agents/domain.md](docs/agents/domain.md).

# Change workflow

- GitHub CI is the PR gate. Locally, run only focused checks: typecheck, lint, fmt, and the tests for the code you changed. Don't run a full suite (`pnpm pr:check`, `pnpm test`, `cargo nextest run`, workspace clippy) locally, except to reproduce a CI failure.
- Opening, reviewing, verifying and landing PRs: follow [docs/agents/landing.md](docs/agents/landing.md).

Draw a diagram where one picture compresses many words: how systems relate, flows, forks. Show UI with screenshots or prototypes, never ASCII.
