# Cluster Locking: Investigation & Design

## Problem Statement

Ployz uses Corrosion (CRDT-based distributed SQLite over QUIC gossip) for state
replication. This gives eventual consistency by default — writes replicate
asynchronously across nodes. The deploy system works around this by acquiring
per-namespace locks on all participant machines via TCP RPC sessions, then
committing all routing state changes in a single Corrosion transaction.

This pattern works, but it's tightly coupled to the deploy lifecycle. Other
operations would benefit from the same guarantee: **lock a resource across the
cluster, perform an operation with the guarantee that you hold exclusive access,
and know that you're reading the latest state**.

## Use Cases

### 1. Namespace Locks for Deploys (existing)

The current system acquires per-machine namespace locks during deploys. The lock
is held for the session lifetime (connection-scoped). This works but is:

- Deploy-specific: the `DeployFrame` protocol mixes lock acquisition with
  deploy commands (`StartCandidate`, `DrainInstance`, etc.)
- Not reusable by other operations that need mutual exclusion on a namespace

### 2. Configuration Changes

Updating service configuration (env vars, resource limits, port mappings)
outside of a full redeploy. If two operators change config concurrently without
coordination, last-write-wins semantics can silently discard changes.

### 3. Namespace Teardown / Drain

Draining all services in a namespace before decommissioning. Without a lock,
a concurrent deploy could start new instances in the namespace while drain
is in progress.

### 4. Machine Lifecycle Operations

Draining a machine (moving all slots elsewhere) requires coordinating with
any in-flight deploys. Currently the deploy system checks machine participation
state, but there's a race window between marking a machine as draining and an
in-flight deploy placing new slots on it.

### 5. Schema Migrations / Data Operations

Coordinating database migrations across a namespace — e.g., "stop all instances,
run migration, restart." Requires exclusive access to ensure no new instances
start during the migration window.

### 6. Secret Rotation

Rotating secrets across a namespace's instances requires a coordinated rollout
— reading current state, generating new secrets, then updating all instances.
Without a lock, partial rotation is possible.

### 7. Read-After-Write Consistency

Sometimes you just need to know your write has propagated. After a deploy
commits, the coordinator returns success, but other nodes may not have the
new routing state yet. An operator running `ployz status` on a different node
might see stale state. A cluster-wide fence/barrier would let you wait until
a specific write has been observed by all (or a quorum of) nodes.

## Current Architecture

### What exists today

```
NamespaceLockManager (local mutex)
  └── per-machine, in-process HashMap<namespace, DeployId>
  └── NamespaceLock (RAII guard, releases on drop)

DeployFrame protocol (TCP, line-delimited JSON)
  └── Open: acquires namespace lock, returns instance snapshot
  └── InspectNamespace, StartCandidate, DrainInstance, RemoveInstance, Close
  └── Session = 1 connection = 1 namespace lock on 1 machine

Deploy coordinator flow:
  1. Sort participants (deadlock avoidance)
  2. Open session to each participant (acquires lock + snapshot)
  3. Revalidate preview under lock
  4. Start candidates, wait for readiness
  5. Atomic commit via Corrosion transaction
  6. Cleanup old instances
  7. Close sessions (releases locks)
```

### Strengths

- Deadlock avoidance via sorted acquisition order
- RAII-based lock release (connection drop = unlock)
- Idempotent operations (StartCandidate, DrainInstance are all idempotent)
- Lock holder tracked by deploy ID for diagnostics

### Weaknesses

- **Deploy-coupled**: Lock acquisition is interleaved with deploy-specific
  operations (instance snapshot, candidate start). Other operations can't
  reuse the lock without opening a deploy session.
- **No heartbeat/lease**: The current design relies on TCP connection liveness.
  A coordinator that hangs holds locks indefinitely. The docs mention a 5s
  heartbeat interval, but the current `DeployFrame` protocol doesn't include
  a heartbeat frame — it relies on TCP keepalive and connection close.
- **No fencing token**: Nothing prevents a stale coordinator from issuing
  commands after its lock has expired (if leases were added). A fencing token
  would let participants reject commands from expired sessions.
- **No read-after-write barrier**: After `commit_deploy()` writes to Corrosion,
  there's no mechanism to wait for the writes to propagate to other nodes.

## Design: General Cluster Lock Service

### Core Idea

Extract locking from the deploy protocol into a standalone cluster lock service.
A cluster lock is a named resource that can be held by exactly one holder across
the entire mesh. The lock is acquired by contacting all relevant machines (or a
coordination service) and provides:

1. Mutual exclusion on a named resource
2. A fencing token for stale-lock detection
3. Optional read-after-write barrier

### Option A: Distributed Lock via All-Machine RPC (extend current approach)

Keep the current model where the coordinator contacts each machine and acquires
a local lock, but generalize it beyond deploys.

```
ClusterLockFrame:
  // client → server
  Acquire { resource: String, holder_id: String, lease_secs: u32 }
  Heartbeat { holder_id: String }
  Release { holder_id: String }

  // server → client
  Acquired { fence_token: u64 }
  Rejected { holder: String, message: String }
  Released
  Error { code: String, message: String }
```

**Lock resource naming:**
- `namespace:{ns}` — namespace-scoped (deploy, config, drain)
- `machine:{id}` — machine-scoped (drain, decommission)
- `global:schema-migration` — arbitrary named locks

**How it works:**
1. Coordinator resolves participant set (e.g., all machines for a namespace lock,
   all machines in the cluster for a global lock)
2. Opens TCP connections to each participant, sorted by machine ID
3. Sends `Acquire` with a lease duration
4. Each participant checks its local `LockManager` — if free, grants the lock
   with a monotonically increasing fence token; if held, rejects
5. Coordinator sends periodic `Heartbeat` to extend the lease
6. On completion, sends `Release`; on timeout, the lease expires and the lock
   is automatically released

**Fence tokens:**
Each machine maintains a monotonic counter per resource. The fence token returned
on `Acquired` is the value at acquisition time. Subsequent operations that depend
on the lock (e.g., Corrosion writes) can include the fence token. If a stale
holder tries to write, participants can reject operations with an outdated fence.

**Pros:**
- Natural extension of the current TCP RPC model
- No new infrastructure (no separate lock service)
- Works with any number of machines
- Lease-based — automatic cleanup on coordinator failure

**Cons:**
- Requires contacting **all** participants for every lock — doesn't scale to
  thousands of machines (though ployz clusters are typically small)
- Split-brain possible: if network partitions, two coordinators on different
  sides could both believe they hold the lock (each only seeing a subset of
  machines)
- No single source of truth — each machine holds its own lock state

### Option B: Single-Leader Lock Service via Corrosion

Use Corrosion itself as the coordination layer. Write a lock-acquisition record
to Corrosion and rely on its replication guarantees.

```sql
CREATE TABLE IF NOT EXISTS cluster_locks (
    resource TEXT NOT NULL PRIMARY KEY,
    holder_id TEXT NOT NULL DEFAULT '',
    fence_token INTEGER NOT NULL DEFAULT 0,
    acquired_at INTEGER NOT NULL DEFAULT 0,
    lease_expires_at INTEGER NOT NULL DEFAULT 0,
    holder_machine_id TEXT NOT NULL DEFAULT ''
);
```

**How it works:**
1. Coordinator writes an `INSERT OR REPLACE` with its holder_id and a lease expiry
2. Other coordinators read the lock before attempting acquisition — if held and
   not expired, they wait or fail
3. Fence token is bumped monotonically by the acquiring coordinator

**Problem:** Corrosion uses CRDTs with last-write-wins semantics. Two concurrent
`INSERT OR REPLACE` operations would both succeed locally and then converge to
whichever has the later timestamp — there's no atomic compare-and-swap. This
means two coordinators could both believe they acquired the lock.

**Verdict:** Corrosion's eventual-consistency model makes it unsuitable as a
lock coordination backend without additional consensus. This option is not
viable as-is.

### Option C: Hybrid — Corrosion for State, RPC for Locking

Keep the all-machine RPC lock (Option A) for mutual exclusion, and add a
Corrosion-based barrier for read-after-write consistency.

**Locking (mutual exclusion):** Use Option A — generalized lock protocol
over TCP RPC, separated from the deploy protocol.

**Read-after-write barrier:** After a coordinator commits a Corrosion
transaction, it can ask each machine "have you seen changes up to version X?"
by querying Corrosion's sync status. The coordinator records the change ID
from its commit, then polls machines until they've all caught up.

```
BarrierFrame:
  // client → server
  WaitForSync { change_id: u64 }

  // server → client
  Synced
  Timeout
```

This splits the two concerns:
- **Locking** = active coordination via direct RPC (strong guarantee)
- **Consistency** = passive observation of Corrosion sync progress

## Recommendation: Option A with Lease + Fence Tokens

Option A is the most pragmatic path forward. It extends the existing model
(which works) without requiring new infrastructure or changing the consistency
model. Here's the concrete design:

### 1. Generalized LockManager

Replace `NamespaceLockManager` with a general-purpose `ClusterLockManager`:

```rust
pub struct ClusterLockManager {
    locks: Arc<Mutex<HashMap<String, LockEntry>>>,
    fence_counters: Arc<Mutex<HashMap<String, u64>>>,
}

struct LockEntry {
    holder_id: String,
    fence_token: u64,
    lease_expires_at: Instant,
}

pub struct ClusterLock {
    manager: Arc<Mutex<HashMap<String, LockEntry>>>,
    resource: String,
}

impl Drop for ClusterLock {
    fn drop(&mut self) {
        // Release lock
    }
}
```

The `NamespaceLockManager` becomes a thin wrapper that formats resource names
as `namespace:{ns}`.

### 2. Separate Lock Protocol

A new TCP listener (or a multiplexed channel on the existing deploy port)
handles lock-specific frames:

```rust
pub enum ClusterLockFrame {
    // client → server
    Acquire {
        resource: String,
        holder_id: String,
        lease_secs: u32,
    },
    Heartbeat {
        resource: String,
        holder_id: String,
    },
    Release {
        resource: String,
        holder_id: String,
    },

    // server → client
    Acquired {
        fence_token: u64,
    },
    Rejected {
        current_holder: String,
        message: String,
    },
    Released,
    Error {
        code: String,
        message: String,
    },
}
```

### 3. Deploy Protocol Migration

The `DeployFrame::Open` currently combines lock acquisition with instance
snapshot loading. With a separate lock service:

1. Coordinator acquires `namespace:{ns}` lock on all machines via `ClusterLockFrame`
2. Coordinator opens deploy sessions (no longer need to acquire lock — just verify
   the fence token)
3. Deploy operations include the fence token; participants reject operations
   with mismatched tokens

This separation means the deploy protocol no longer needs `Open`/`Close` for
locking — it can focus purely on instance management.

### 4. Lease Expiry

Each `LockEntry` has a `lease_expires_at`. A background task periodically
scans for expired leases and releases them. The heartbeat extends the lease.
Reasonable defaults:

- Default lease: 30 seconds
- Heartbeat interval: 10 seconds (well within the 30s lease)
- Max lease extension: 5 minutes (prevent runaway holders)

### 5. Read-After-Write Barrier (optional, separate concern)

For the read consistency use case, add a `WaitForSync` RPC that blocks until
the local Corrosion node has caught up to a specific change. This doesn't
require locking — it's a polling operation:

```rust
pub async fn wait_for_sync(
    client: &CorrClient,
    timeout: Duration,
) -> Result<()> {
    let deadline = Instant::now() + timeout;
    loop {
        let health = client.health().await?;
        if health.gaps == 0 {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(Error::operation("wait_for_sync", "timeout"));
        }
        sleep(Duration::from_millis(200)).await;
    }
}
```

This can be exposed as a `ployz sync` CLI command and used internally
after deploy commits.

## Implementation Plan

### Phase 1: Extract Lock Protocol

1. Create `ClusterLockManager` as a generalization of `NamespaceLockManager`
2. Add `ClusterLockFrame` protocol alongside `DeployFrame`
3. Add lease expiry and heartbeat support
4. Namespace locks delegate to `ClusterLockManager` with `namespace:{ns}` keys

### Phase 2: Separate Lock from Deploy

1. Deploy coordinator acquires cluster lock before opening deploy sessions
2. Deploy sessions no longer acquire locks (they verify fence tokens)
3. `DeployFrame::Open` becomes a pure session-open (no lock acquisition)

### Phase 3: General Lock Commands

1. `ployz lock acquire <resource> [--lease 30s]` — CLI for manual locking
2. `ployz lock release <resource>`
3. `ployz lock list` — show held locks across the cluster
4. `ployz sync` — wait for Corrosion convergence (read-after-write barrier)

### Phase 4: Higher-Level Operations

1. `ployz namespace drain <ns>` — acquires namespace lock, drains all services
2. `ployz machine drain <id>` — acquires machine lock, migrates slots
3. Coordinated secret rotation, schema migration hooks

## Open Questions

1. **Lock granularity:** Should we support sub-namespace locks (e.g.,
   `namespace:prod:service:api`)? Adds complexity but enables concurrent
   deploys of independent services within the same namespace.

2. **Quorum vs. all-machine:** For large clusters, requiring all machines
   to acknowledge is slow and fragile. A quorum-based approach (majority of
   machines) trades strict mutual exclusion for availability. For ployz's
   target cluster sizes (< 50 machines), all-machine is likely fine.

3. **Lock contention UX:** When a lock is held, what should the CLI show?
   "Namespace 'prod' is locked by deploy 'abc123' on machine 'xyz'. Waiting..."
   with a spinner and timeout? Or fail immediately?

4. **Persistent locks:** Should lock state survive machine restarts? Currently
   locks are purely in-memory (lost on restart). For long-running operations
   (multi-hour migrations), persistent locks with crash recovery might matter.
   However, lease-based expiry handles most crash scenarios gracefully.
