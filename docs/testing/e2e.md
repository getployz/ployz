# E2E Strategy

Ployz E2E tests live in `crates/ployz-e2e`. They are the long-running system
harness and should be reserved for behavior that cannot be tested meaningfully
below E2E.

Use E2E when the value comes from crossing real boundaries: installed payloads,
multiple node containers, daemon processes, SSH bootstrap, real network
partitions, runtime containers, gateway/DNS/ACME behavior, or real ZFS.

Do not add E2E scenarios for command policy, state transitions, rendering,
store projections, NATS subject construction, or failure classification that can
be covered with memory stores, fake backends, command-handler tests, or
crate-level integration tests.

## Current Scenario Set

There is no real-boundary E2E suite yet. `just e2e` currently runs
`cargo test -p ployz-e2e`, which contains in-process product acceptance tests.

Those tests are useful while the operation spine is being shaped, but they are
not proof of installed payloads, daemon processes, SSH bootstrap, runtime
containers, gateway behavior, or real storage. Fake-backed scenarios should move
down into crate-level tests as the MVP runtime surface stabilizes.

## Coverage That Belongs Below E2E

- Machine add must not promote storage authority: test through NATS/store/daemon
  integration surfaces by asserting default replica policy remains R=1 until an
  explicit storage-promotion primitive exists.
- Drain, standby, activate, and membership lifecycle transitions: test in daemon
  command-handler and orchestrator tests.
- Unreachable peer foreground failures: test NATS RPC no-responder and timeout
  classification through daemon/NATS tests.
- Destroy-with-dead-peer semantics: test handler behavior unless a real teardown
  substrate bug requires E2E coverage.
- Managed volume fake-ZFS behavior: test below E2E; reserve E2E storage coverage
  for real ZFS.

## Adding A Scenario

Before adding a new scenario, identify the real boundary it protects and the
lower-level test that would otherwise be insufficient. If the assertion can be
made with a fake backend or memory store, add it there instead.

Scenario names should describe the substrate behavior being protected, not the
implementation detail being exercised.
