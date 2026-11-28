# Workspace

- `core/` owns the engine, CLI, daemon, SDK, native helpers, and releases. Run Cargo there; apply relevant `core/CODING_STANDARDS.md` rules when changing core code.
- `dashboard/` owns Ployz Cloud: web, worker, durable workflows, and self-host packaging. Run pnpm there.
- `minter/` holds the GitHub App's key for hosted Cloud and gives Cloud only read tokens and narrow build operations. It sits outside Cloud's trust domain and never imports Cloud code. Run pnpm there.
- Before designing Engine behavior, including Cloud code that drives it, read the bets in `core/DESIGN.md`. Name each bet your change touches in the PR body, and the choice it changed. If you cross a red flag, argue it there. Dashboard UI follows `dashboard/DESIGN.md`.
- Use the terms defined in `core/CONTEXT.md` (Engine) and `dashboard/CONTEXT.md` (Cloud) rather than synonyms. Work across the Engine–Cloud boundary reads both.
- `docs/user/` describes shipped behavior from the user's point of view. Check it before deciding how something works today, and update it when user-visible behavior changes.
- Prove changes on the real artifact: dashboard UI with the `verify-dashboard` skill (add `SERVERS=1` for runtime UI), and anything needing real Machines (daemon, Docker, WireGuard, ZFS, moves) with the `verify-server` skill, before reaching for cloud VMs.

# Change workflow

- GitHub CI is the PR gate. Locally, run only focused checks: typecheck, lint, fmt, and the tests for the code you changed. Don't run a full suite (`pnpm pr:check`, `pnpm test`, `cargo nextest run`, workspace clippy) locally, except to reproduce a CI failure. Focused checks include the easy-to-miss ones CI runs: rustfmt, the real-wait lint and the dashboard's static-analysis tests.
- Watch every PR you open or queue until it merges. The watch wakes on checks, conflicts, merges and queue removal.
- Before retrying a failure as a flake, run the test with its siblings on main. If main fails too, fix main.
- A bug you find outside your task: score how often users would hit it (F) and how bad it is when they do (S), each 1 to 10. S is 10 for data loss, a secret exposed or a security hole (see the Cloud security bar), 9 for an outage that doesn't heal, 5 for a broken flow with a workaround, 2 for cosmetic. Fix it only when F×S is 15 or more, or S is 9 or more. Otherwise file an issue with both scores and leave the code alone.

# Proof

- Prove a change by using it (`verify-server`, `verify-dashboard`) and, for ordering and concurrency, by model-checking it (the `verify-*` Stateright models in `core/crates/ployz-verify`).
- Write a test only when it goes red on a bug a user would hit and stays green through a refactor. A test that mirrors source text, copy, constants or call counts is tautological: delete it. The same goes for a label that restates its title and a component that only forwards props.
- A change to a pinned image's source publishes the image from its branch, pins that digest, and is proven on the published image, never a local build.

# Cloud security bar

- In scope: a breach of Ployz Cloud must not let the attacker mass-leak customer secrets or volume and database data, or add a Server or Device. Secrets and volume data reach only containers whose spec a user signed: with a passkey, the full-access CLI, or a verified branch build.
- Accepted: a compromised Cloud that swaps or reorders what runs without getting data, a tampered approval page shown to people approving during the breach, and Sealed values that log masking misses on their way to Cloud's log history: encoded copies, values under 6 bytes, and values cut by Docker's 16 KiB line split. Name these once as residuals in the PR body and move on, without review rounds or fixes.
- A security hole is a way to get secrets or volume data out, or to add a Server or Device, from Cloud alone. Point reviewers and swarm testers at this bar, and judge their findings by it.

Draw a diagram where one picture compresses many words: how systems relate, flows, forks. Show UI with screenshots or prototypes, never ASCII.
