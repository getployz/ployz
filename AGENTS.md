# Workspace

- `core/` owns the engine, CLI, daemon, SDK, native helpers, and releases. Run Cargo there; apply relevant `core/CODING_STANDARDS.md` rules when changing core code.
- `dashboard/` owns Ployz Cloud: web, worker, durable workflows, and self-host packaging. Run pnpm there.
- Before designing a feature, read the affected project's `DESIGN.md`. A change that fights one of its bets needs a `DESIGN.md` amendment justifying the exception — or a redesign.
- `docs/user/` describes shipped behavior from the user's point of view. Check it before deciding how something works today, and update it when user-visible behavior changes.
- When changing domain behavior, terminology, or ownership boundaries, follow [docs/agents/domain.md](docs/agents/domain.md).

# Change workflow

Draw a diagram where one picture compresses many words: how systems relate, flows, forks. Show UI with screenshots or prototypes, never ASCII.
