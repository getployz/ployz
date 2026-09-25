# Workspace

- `core/` owns the engine, CLI, daemon, SDK, native helpers, and releases. Run Cargo there; apply relevant `core/CODING_STANDARDS.md` rules when changing core code.
- `dashboard/` owns Ployz Cloud: web, worker, durable workflows, and self-host packaging. Run pnpm there.
- Before designing a feature, read the affected project's `DESIGN.md`. A change that fights one of its bets needs a `DESIGN.md` amendment justifying the exception — or a redesign.
- When changing domain behavior, terminology, or ownership boundaries, follow [docs/agents/domain.md](docs/agents/domain.md).

# Change workflow

For `$implement`, `$four-axis-review` supersedes `$code-review`. After implementation, follow its incremental rerun loop until all four axes pass. Do not run `$four-axis-review` on docs, CI, research, or scripts.

Prefer simple diagrams. Use `$i-have-adhd` output.
