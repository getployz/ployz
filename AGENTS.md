# Workspace

- `core/` owns the engine, CLI, daemon, SDK, native helpers, and releases. Run Cargo there; apply relevant `core/CODING_STANDARDS.md` rules when changing core code.
- `dashboard/` owns Ployz Cloud: web, worker, durable workflows, and self-host packaging. Run pnpm there.
- `minter/` holds the GitHub App's key for hosted Cloud and gives Cloud only read tokens and narrow build operations. It sits outside Cloud's trust domain and never imports Cloud code. Run pnpm there.
- Before designing Engine behavior, including Cloud code that drives it, read the bets in `core/DESIGN.md`. Name each bet your change touches in the PR body, and the choice it changed. If you cross a red flag, argue it there. Dashboard UI follows `dashboard/DESIGN.md`.
- Use the terms defined in `core/CONTEXT.md` (Engine) and `dashboard/CONTEXT.md` (Cloud) rather than synonyms. Work across the Engine–Cloud boundary reads both.
- `docs/user/` describes shipped behavior from the user's point of view. Check it before deciding how something works today, and update it when user-visible behavior changes.
- Prove changes on the real artifact: dashboard UI with the `verify-dashboard` skill (add `SERVERS=1` for runtime UI), and anything needing real Machines (daemon, Docker, WireGuard, ZFS, moves) with the `verify-server` skill, before reaching for cloud VMs.

# Change workflow

- GitHub CI is the PR gate. Locally, run only focused checks: typecheck, lint, fmt, and the tests for the code you changed. Don't run a full suite (`pnpm pr:check`, `pnpm test`, `cargo nextest run`, workspace clippy) locally, except to reproduce a CI failure.
- Opening, reviewing, verifying and landing PRs: follow [docs/agents/landing.md](docs/agents/landing.md).
- A bug you find outside your task: score how often users would hit it (F) and how bad it is when they do (S), each 1 to 10. S is 10 for data loss, a secret exposed or a security hole (see the Cloud security bar), 9 for an outage that doesn't heal, 5 for a broken flow with a workaround, 2 for cosmetic. Fix it only when F×S is 15 or more, or S is 9 or more. Otherwise file an issue with both scores and leave the code alone.

# Proof

- Prove a change by using it (the swarm in [docs/agents/landing.md](docs/agents/landing.md), `verify-server`, `verify-dashboard`) and, for ordering and concurrency, by model-checking it (the `verify-*` Stateright models in `core/crates/ployz-verify`).
- Write a test only when it goes red on a bug a user would hit and stays green through a refactor. A test that mirrors source text, copy, constants or call counts is tautological: delete it. The same goes for a label that restates its title and a component that only forwards props.

# Cloud security bar

- Ployz Cloud is designed to be a poor target: it holds no customer secrets, and a Server acts only on what it can check itself. Whoever takes over Cloud must still be unable to read secrets, run code of their choosing on a Server, take data off a Server, or add a Server or Device. That is the bar.
- A security hole is a way across that bar. A compromised Cloud that mislabels, reorders, delays or drops an action a person approved stays under it: name it once as a residual in the PR body and move on, without review rounds or fixes. Point reviewers and swarm testers at the bar, and judge their findings by it.

Draw a diagram where one picture compresses many words: how systems relate, flows, forks. Show UI with screenshots or prototypes, never ASCII.
