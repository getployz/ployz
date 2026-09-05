# Validated Rust domain invariants

Implement all fourteen accepted groups in the production Rust invalid-state audit, plus known host-port placement admission, as one coherent greenfield change. The accepted scope is the task graph in [index.md](index.md); the coordinator's audit report at /home/codex/.codex/worktrees/d5ab/ployz2/docs/audits/rust-invalid-states/report.md and its three synthesis reports hold source evidence and rejected claims. Ticket descriptions specify end-to-end acceptance, not suggested partial steps.

## Constraints

- Greenfield, no users: change Rust APIs, stored/wire shapes and generated SDK contracts directly. Establish real invariants with private aggregates, enums, required payloads, derived facts and checked boundary admission. Avoid compatibility layers and generic typestate frameworks.
- Preserve DESIGN.md and CONTEXT.md: observer-relative, stale/partial observations, duplicate names, optimistic Machine-local resources, bounded operations, no global authority or general rollback.
- Keep raw external input distinct from validated domain values. Runtime checks establish content/correspondence; privacy preserves those facts in Rust. Async lifecycle permission requires synchronization through side effects, not just a copied type token.
- Explicit exclusions: no new policy forbidding ordinary direct create during Joining; no speculative ZFS accounting equality; no new fixed-port/current-peer security promise for image transfer; no replacement rollback or expanded substep contract. Global convergence remains Participating-only. Smaller unrelated parser cleanups are outside this cut.

## Ticket execution

Each implementation ticket owns one complete invariant through core types, constructors/serde, consumers, CLI/SDK projection and meaningful regression checks. Dependencies define a task graph; only dispatch its current ready frontier. Some dependencies intentionally serialize edits to shared domain files. Target four to six active implementers. Each implementer has its own worktree/branch; a merger agent integrates completed ticket commits into `feat/rust-domain-invariants` and resolves conflicts against completed acceptance criteria. Update generated files when the ticket changes their contract, then regenerate once more at integration.

Control files live at /home/codex/.codex/worktrees/d5ab/ployz2/.scratch/rust-domain-invariants. Exploration notes live outside the repository at /home/codex/.cache/ployz-domain-invariants-notes. Agents communicate primarily through these pointers and their commits/handoff files. Coordinator owns ticket states. Use named local ticket IDs in the PR; no GitHub issues were supplied, so do not invent closing issue numbers.

## Verification and completion

The user-approved ticket scope includes regression checks at existing public constructor/deserialization, planning, local operation and client interfaces. Read the repository TDD guidance; use red→green at these established seams where possible. Name each rung and exact test. Follow evidence/product-paths.tsv and report necessary lowest honest gap updates to the coordinator, which consolidates them without parallel edits. Adapt existing tests to changed public contracts; tests were excluded from the audit, not from implementation verification.

Use regular affected-target typechecks and focused tests; final full suite once after integration. Follow `$implement` for Rust and the AGENTS.md `$four-axis-review` override. The final spec-level gate is four isolated reviewers, fixes, incremental stale-axis triage and selective reruns until all axes pass on the complete branch. Ticket agents submit tested commits; the final integrated review is the authoritative four-axis gate.

Done means every T01–T18 acceptance criterion implemented, T00 decision resolved, generated SDK contracts consistent, meaningful regression evidence recorded, Fast CI checks passed, four review axes current/pass, one PR ready for human review, and task-owned implementer worktrees cleaned. Do not merge into main or deploy.

## Build resources

Use the shared Cargo target directory `/home/codex/.cache/ployz-domain-invariants-target` with CARGO_BUILD_JOBS=2, CARGO_INCREMENTAL=0, CARGO_PROFILE_DEV_DEBUG=0 and CARGO_PROFILE_TEST_DEBUG=0. Dependency builds are shared; Cargo may serialize commands. Do not launch redundant parallel builds or delete other tasks' caches/worktrees.
