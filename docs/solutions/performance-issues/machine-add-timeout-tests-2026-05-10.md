---
title: Machine Add Timeout Tests Should Not Wait On Production Deadlines
date: 2026-05-10
category: docs/solutions/performance-issues/
module: machine add orchestration tests
problem_type: performance_issue
component: testing_framework
symptoms:
  - "just test-all spent about 32 seconds inside ployzd lib tests after compilation"
  - "machine_add_requires_sync_connected_for_running_joiner took about 30 seconds by waiting for the production remote-ready deadline"
  - "interrupted_machine_add_is_marked_interrupted_on_startup took about 15 seconds by exercising real SSH cleanup timeout behavior"
root_cause: async_timing
resolution_type: test_fix
severity: medium
tags:
  - machine-add
  - slow-tests
  - timeout-policy
  - fake-ssh
  - ployzd
---

# Machine Add Timeout Tests Should Not Wait On Production Deadlines

## Problem

`just test-all` was slow even after compilation because a small number of
`ployzd` unit tests intentionally walked production timeout paths. The suite was
paying 30 seconds for remote readiness and about 15 seconds for SSH cleanup when
the behavior under test did not require real wall-clock deadlines.

## Symptoms

- `time -p just test-all` initially reported about 66 seconds wall clock, with
  compilation around 31 seconds and `ployzd` lib tests around 32 seconds.
- Warm `cargo test` still took about 35 seconds because the slow behavior was in
  test execution, not only in compilation.
- Per-test timing isolated `machine_add_requires_sync_connected_for_running_joiner`
  at about 30.8 seconds and `interrupted_machine_add_is_marked_interrupted_on_startup`
  at about 14.6 seconds.

## What Didn't Work

- Treating the suite as generally slow was too broad. The timing showed a few
  tests dominated the wall clock.
- Shrinking production constants would have made real machine-add behavior less
  patient in production, which was the wrong contract to change.
- A global test timeout override leaked policy across unrelated paths, including
  NATS waits, and was brittle under parallel test execution.
- Removing remote cleanup from the `BootstrapPublished` recovery stage made the
  test faster but regressed the recovery model: the stage can mean the
  `MeshBootstrap` RPC was sent before durable progress advanced.

## Solution

Keep production deadlines as the default and add an instance-scoped wait policy
that tests can inject only into the machine-add remote readiness path.

Production code still uses:

```rust
pub(super) fn production_remote_ready_wait_policy() -> RemoteReadyWaitPolicy {
    RemoteReadyWaitPolicy::new(
        DEFAULT_REMOTE_READY_TIMEOUT,
        REMOTE_READY_POLL_INTERVAL,
        REMOTE_READY_RPC_TIMEOUT,
    )
}
```

Tests that need to prove the timeout branch can now ask for a tiny policy on
their `DaemonState` instance:

```rust
state.machine_add_remote_ready_wait_policy =
    Some(RemoteReadyWaitPolicy::new(
        Duration::from_millis(50),
        Duration::from_millis(1),
        Duration::from_millis(20),
    ));
```

The policy is copied into `MachineAddContext`, so it travels with a single
machine-add operation instead of changing process-wide behavior.

For interrupted recovery tests, use fake SSH when the assertion is about daemon
recovery bookkeeping and cleanup intent. That keeps coverage on the recovery
contract without waiting for real SSH command timeouts:

```rust
let ssh = Arc::new(fake_ssh_recorder());
let state = DaemonState::with_dependencies_for_test(store.clone(), Arc::clone(&ssh));
```

The recovery implementation also keeps local bootstrap membership cleanup and
remote cleanup independent. If there is no active mesh, local membership cleanup
records a skipped observation, but remote cleanup is still attempted when the
operation stage is eligible.

## Why This Works

The tests were not trying to validate that 30 seconds is exactly 30 seconds.
They were validating behavior at the end of a wait: the joiner was not ready, so
machine add must fail and roll back cleanly. A short, operation-scoped timeout
exercises the same branch while preserving production defaults.

The fake SSH recovery test separates intent from transport timing. The daemon
recovery behavior is covered by asserting that cleanup was attempted for the
interrupted machine, while lower-level SSH timeout behavior remains the
responsibility of SSH-specific tests.

Keeping `BootstrapPublished` remote-cleanup eligible matches the failure window:
the target may already have received the bootstrap RPC, even if the durable stage
did not advance. Startup recovery should prefer a visible cleanup attempt over
assuming nothing happened.

## Prevention

- Time slow suites before changing broad test commands; isolate slow test bodies
  with `cargo test -- --nocapture` and per-test timing.
- Avoid production sleeps and production timeout constants in unit tests unless
  the deadline itself is the behavior under test.
- Prefer instance-scoped test policies over global overrides, especially for
  async code that may run tests concurrently.
- Use fakes for external transports when the test is about orchestration,
  recovery notes, or cleanup intent rather than the transport implementation.
- When speeding up recovery tests, keep stage eligibility faithful to what may
  already have happened externally.

## Related Issues

- `docs/plans/2026-05-10-001-fix-fast-machine-add-timeout-tests-plan.md`
  documents the implementation plan and review findings for this fix.
