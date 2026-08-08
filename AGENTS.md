# Agent Instructions

Ployz is a small-cluster orchestrator without a replicated core. Read
`VISION.md` and `CONTEXT.md` for product or domain work, and the relevant ADRs plus
`docs/architecture/code-map.md` before changing architecture, state ownership,
runtime ownership, repository structure, or test placement.

v2 is Corrosion rows over HTTP/JSON/SSE on a WireGuard mesh: no quorum,
sequencer, or NATS. One advisory preferred controller serializes cluster
mutations in memory, and followers forward mutations to it. Each node uses
Duroxide with local SQLite only for its own bounded host prepare/retire work.
Feature parity with Uncloud is the floor; simplify guarantees and
implementation, not the product surface.

Prefer the simplest implementation that satisfies the product requirement;
accept reasonable limitations and recoverable failure instead of adding
machinery for hypothetical robustness. Default to the preferred controller
querying reality, computing a plan, and dispatching bounded idempotent effects
to nodes; after controller loss, retry from reality rather than adding leases,
heartbeats, takeover, history migration, or recovery protocols unless a
concrete data-safety requirement demands them.

Prefer existing libraries, binaries, and OS primitives when they reduce total
maintained code and concepts. Avoid speculative abstractions, provider seams,
and second implementations that do not yet exist.

Follow the surrounding Rust style and the contributor code map. Keep domain
logic independent of transport and process wiring, bound external I/O, and
write comments about current behaviour and invariants rather than change
history.

Use GitHub issues. For Wayfinder, implementation, delegation, and review, load
the matching `.agents/skills/*/SKILL.md`; those files are canonical. Run
focused tests while working and the relevant `.github/workflows/pr.yml` gates
before landing.
