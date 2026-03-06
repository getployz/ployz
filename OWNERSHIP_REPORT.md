# Ployz — State-of-Ownership Report

> Generated: 2026-03-06 | Scope: full codebase audit | No code changes made

---

## 1. Executive Summary

Ployz is a distributed mesh network orchestration daemon built on WireGuard and Corrosion (distributed SQLite). The codebase uses **3 Mutex instances**, **7 distinct Arc-wrapped types**, **0 RwLock/RefCell/Rc**, and **4 channel patterns**. Ownership is generally clean: a single-threaded daemon loop (`DaemonState`) owns the `Mesh` orchestrator, which in turn owns its backends and spawned tasks. Shared state exists primarily at the adapter layer to allow `Clone`-able enum dispatch across async task boundaries.

---

## 2. Component Ownership Map

### 2.1 DaemonState — the root owner

| Field | Type | Owner | Sharing |
|-------|------|-------|---------|
| `data_dir` | `PathBuf` | DaemonState | Not shared |
| `identity` | `Identity` | DaemonState | Read-only refs to handlers |
| `mode` | `Mode` | DaemonState | Not shared |
| `active` | `Option<ActiveMesh>` | DaemonState | Exclusive via `&mut self` |

**File:** `src/daemon/mod.rs:25-30`

`DaemonState` is the single root of ownership. It lives in the daemon's command loop (`src/bin/ployzd.rs:119`) and is never shared — all handler calls go through `&mut self`. This is the backbone of the entire system.

### 2.2 ActiveMesh — owned by DaemonState

| Field | Type | Owner |
|-------|------|-------|
| `config` | `NetworkConfig` | ActiveMesh (value) |
| `mesh` | `Mesh` | ActiveMesh (value) |

**File:** `src/daemon/mod.rs:20-23`

Stored as `Option<ActiveMesh>` inside `DaemonState`. Created in `start_mesh()`, consumed by `take()` during `mesh_down`/`mesh_destroy`.

### 2.3 Mesh (orchestrator) — owned by ActiveMesh

| Field | Type | Owner | Sharing |
|-------|------|-------|---------|
| `phase` | `Phase` | Mesh | Not shared |
| `network` | `WireguardDriver` | Mesh | **Cloned** into peer_sync task |
| `store` | `StoreDriver` | Mesh | **Cloned** via `store()` accessor, and into bootstrap_gate |
| `tasks` | `Option<TaskSet>` | Mesh | Not shared |
| `bootstrap_interval` | `Duration` | Mesh | Not shared |
| `connection_timeout` | `Duration` | Mesh | Not shared |
| `service_ready_timeout` | `Duration` | Mesh | Not shared |

**File:** `src/mesh/orchestrator.rs:23-31`

The `store()` method (`line 60`) returns `self.store.clone()` — this is how handlers access the store for queries. Both `WireguardDriver` and `StoreDriver` derive `Clone` and contain `Arc`-wrapped internals, making this cheap.

### 2.4 TaskSet — owned by Mesh

| Field | Type | Owner |
|-------|------|-------|
| `tasks` | `JoinSet<()>` | TaskSet |
| `cancel` | `CancellationToken` | TaskSet (clone shared with spawned tasks) |

**File:** `src/tasks/mod.rs:19-22`

Created during `Mesh::up()` at `orchestrator.rs:115`. The `CancellationToken` is cloned once and given to the `run_peer_sync_task` future. Stopped via `tasks.stop()` during `detach`/`destroy`.

### 2.5 PeerStateMap — owned by peer_sync task

| Field | Type | Owner |
|-------|------|-------|
| `peers` | `HashMap<MachineId, PeerState>` | PeerStateMap |

**File:** `src/mesh/peer_state.rs`

Purely task-local. Created inside `run_peer_sync_task`, never escapes. This is the cleanest ownership in the codebase.

### 2.6 Identity — owned by DaemonState

| Field | Type |
|-------|------|
| `machine_id` | `MachineId` |
| `public_key` | `PublicKey` |
| `private_key` | `PrivateKey` |

**File:** `src/node/identity.rs:36`

Loaded once at daemon startup (`ployzd.rs:95`). Individual fields are cloned into backend constructors (e.g., `private_key.clone()` at `daemon/mod.rs:98,111`). Identity itself is never `Arc`-wrapped.

---

## 3. Shared State Inventory

### 3.1 Arc Usage (complete list)

| # | Wrapped Type | Declared At | Created At | Cloned Where | Purpose |
|---|-------------|------------|-----------|-------------|---------|
| 1 | `Arc<MemoryWireGuard>` | `backends.rs:18` | `daemon/mod.rs:87`, `tests/lifecycle.rs:32` | Via `WireguardDriver::clone()` into peer_sync task | Shared mock WG interface |
| 2 | `Arc<DockerWireGuard>` | `backends.rs:19` | `daemon/mod.rs:104` | Via `WireguardDriver::clone()` | Shared Docker WG interface |
| 3 | `Arc<HostWireGuard>` | `backends.rs:20` | `daemon/mod.rs:115` | Via `WireguardDriver::clone()` | Shared host WG interface |
| 4 | `Arc<MemoryStore>` | `backends.rs:56` | `daemon/mod.rs:89`, `tests/lifecycle.rs:34` | Via `StoreDriver::clone()` | Shared mock store |
| 5 | `Arc<MemoryService>` | `backends.rs:57` | `daemon/mod.rs:90`, `tests/lifecycle.rs:33` | Via `StoreDriver::clone()` | Shared mock service control |
| 6 | `Arc<DockerCorrosion>` | `backends.rs:61` | `daemon/mod.rs:177` | Via `StoreDriver::clone()` | Shared Docker Corrosion lifecycle |
| 7 | `CorrosionStore` (contains internal `Arc`) | `backends.rs:60` | `daemon/mod.rs:170` | Via `StoreDriver::clone()` | HTTP client to Corrosion API |

### 3.2 Mutex Usage (complete list)

| # | Type | Field | File | Line | Lock Method | Held Across Await? |
|---|------|-------|------|------|-------------|-------------------|
| 1 | `Mutex<StoreInner>` | `MemoryStore::inner` | `adapters/memory/store.rs` | 10 | `lock_inner()` at line 42 | **No** — sync Mutex, all methods return before await |
| 2 | `Mutex<WgInner>` | `MemoryWireGuard::inner` | `adapters/memory/wireguard.rs` | 7 | `lock_inner()` at line 37 | **No** |
| 3 | `Mutex<WgBackend>` | `HostWireGuard::backend` | `adapters/wireguard/host.rs` | 25 | `lock_backend()` at line 71 | **No** |

All three use `std::sync::Mutex` (not `tokio::sync::Mutex`) and follow the identical pattern:
- Private `inner`/`backend` field
- Private `lock_*()` helper with poison recovery
- Lock acquired and released within a single synchronous block

### 3.3 RwLock / RefCell / Rc

**None found anywhere in the codebase.**

### 3.4 CancellationToken

| Token | Created At | Cloned To | Purpose |
|-------|-----------|-----------|---------|
| Daemon-level | `ployzd.rs:98` | Listener task (line 101), Ctrl-C handler (line 109) | Graceful daemon shutdown |
| TaskSet-level | `tasks/mod.rs:26` | `run_peer_sync_task` via `orchestrator.rs:127` | Graceful task cancellation on mesh down |

---

## 4. Concurrency Analysis: Genuine vs. Convenience

### 4.1 `Arc<MemoryWireGuard>` + `Mutex<WgInner>` — **Convenience (testing only)**

The `MemoryWireGuard` is a mock used exclusively in `Mode::Memory` and tests. The `Arc` exists because `WireguardDriver` is an enum that must be `Clone` for the `Mesh` to pass it into spawned tasks. The `Mutex` protects mock state. In practice, the daemon command loop is single-threaded and the only concurrent access is from the `peer_sync` task calling `set_peers`.

**Verdict:** The Mutex is structurally required (Arc forces Send+Sync) but contention is near-zero. A single peer_sync task and the daemon loop are the only two accessors.

### 4.2 `Arc<MemoryStore>` + `Mutex<StoreInner>` — **Convenience (testing only)**

Same pattern. The `MemoryStore` is wrapped in `Arc` because `StoreDriver` must be `Clone`. The Mutex guards the HashMap of machines and subscriber list. Accessed from:
1. Daemon command loop (upsert/delete/list via handlers)
2. `peer_sync` task (subscribe_machines)
3. Tests (direct manipulation)

**Verdict:** Low contention. Two writers max (daemon loop + test harness). The subscriber broadcast uses `try_send` to avoid blocking under the lock.

### 4.3 `Arc<HostWireGuard>` + `Mutex<WgBackend>` — **Genuine concurrent access**

This wraps the actual WireGuard kernel/userspace API. Accessed from:
1. `Mesh::up()` / `Mesh::destroy()` (daemon command path)
2. `peer_sync` task calling `set_peers`

**Verdict:** Genuine concurrent access between daemon lifecycle operations and background sync. The Mutex is correctly sized — operations are fast (system calls to configure WG interface).

### 4.4 `Arc<DockerWireGuard>` — **No internal Mutex**

Docker WireGuard uses external Docker CLI commands. No internal mutable state protected by Mutex. The Arc exists for `Clone` semantics in the enum.

**Verdict:** Stateless bridge to Docker. Arc is for sharing, not synchronization.

### 4.5 `Arc<DockerCorrosion>` / `Arc<MemoryService>` — **No internal Mutex**

Service lifecycle adapters. DockerCorrosion manages a Docker container; MemoryService is a mock. Neither has internal Mutex (DockerCorrosion uses Docker CLI; MemoryService likely uses atomics or is stateless).

**Verdict:** Arc for sharing across trait dispatch, not for synchronized state.

---

## 5. Channel Map

### 5.1 Command Channel (Request-Reply)

```
                    mpsc::channel::<IncomingCommand>(32)

  [UnixListener]  ──tx.clone()──►  [per-connection task]
       │                                    │
       │                              tx.send(IncomingCommand)
       │                                    │
       ▼                                    ▼
  socket accept                     ┌─────────────────┐
       │                            │  cmd_rx.recv()   │
       │                            │  (daemon loop)   │
       │                            │  ployzd.rs:137   │
       │                            └────────┬────────┘
       │                                     │
       │                              state.handle(req)
       │                                     │
       │                            reply_tx.send(resp)
       │                                     │
       │                            ┌────────▼────────┐
       │                            │  oneshot reply   │
       │                            │  (per request)   │
       │                            └─────────────────┘
```

- **Created:** `ployzd.rs:99`
- **Sender:** `listener.rs:37` (cloned per connection)
- **Receiver:** `ployzd.rs:137` (single consumer — daemon loop)
- **Capacity:** 32 pending commands
- **Data:** `IncomingCommand { request: DaemonRequest, reply: oneshot::Sender<DaemonResponse> }`

### 5.2 Machine Event Subscription (MemoryStore)

```
  [MemoryStore::upsert_machine / delete_machine]
       │
       │  broadcast() — try_send to all subscribers
       │
       ├──► mpsc::Sender<MachineEvent> ──► [peer_sync task]
       ├──► mpsc::Sender<MachineEvent> ──► [potential future subscriber]
       └──► ...
```

- **Created:** `adapters/memory/store.rs:99` (per `subscribe_machines()` call)
- **Sender:** Stored in `StoreInner.subscribers: Vec<mpsc::Sender<MachineEvent>>`
- **Receiver:** Returned to caller, consumed in `run_peer_sync_task`
- **Capacity:** 64 per subscriber
- **Cleanup:** Dead senders pruned on broadcast (`retain` at line 51)

### 5.3 Machine Event Subscription (CorrosionStore)

```
  [Corrosion SQLite subscription stream]
       │
       │  tokio::spawn — bridge task
       │
       └──► mpsc::Sender<MachineEvent> ──► [peer_sync task]
```

- **Created:** `adapters/corrosion/mod.rs:174`
- **Sender:** Owned by spawned bridge task
- **Receiver:** Returned to caller
- **Capacity:** 64
- **Lifecycle:** Bridge task exits when receiver is dropped

### 5.4 Oneshot Reply (per command)

- **Created:** `listener.rs:63` (per incoming connection)
- **Sender:** Packed into `IncomingCommand`, consumed by daemon loop at `ployzd.rs:140`
- **Receiver:** Awaited by connection handler at `listener.rs:76`

---

## 6. Component Dependency Graph

```
                          ┌─────────────┐
                          │   ployzd    │  (binary entry point)
                          │ ployzd.rs   │
                          └──────┬──────┘
                                 │ owns
                          ┌──────▼──────┐
             ┌───────────►│ DaemonState │◄───────────┐
             │            │ daemon/mod  │            │
             │            └──────┬──────┘            │
             │                   │ owns               │
             │            ┌──────▼──────┐            │
             │            │ ActiveMesh  │            │
             │            │ daemon/mod  │            │
             │            └──────┬──────┘            │
             │                   │ owns               │
        ┌────┴────┐       ┌──────▼──────┐      ┌────┴────────┐
        │Identity │       │    Mesh     │      │NetworkConfig │
        │node/    │       │orchestrator │      │store/network │
        │identity │       └──┬──┬──┬────┘      └─────────────┘
        └─────────┘          │  │  │
              ┌──────────────┘  │  └──────────────┐
              │ owns            │ owns             │ owns
    ┌─────────▼──────┐  ┌──────▼──────┐  ┌───────▼───────┐
    │WireguardDriver │  │ StoreDriver │  │   TaskSet     │
    │  backends.rs   │  │ backends.rs │  │  tasks/mod    │
    │  (Clone/Arc)   │  │ (Clone/Arc) │  │               │
    └───────┬────────┘  └──────┬──────┘  └───────┬───────┘
            │                  │                  │ spawns
    ┌───────┴────────┐  ┌─────┴──────┐  ┌───────▼───────┐
    │ Arc<Memory/    │  │ Arc<Memory │  │run_peer_sync  │
    │  Docker/Host   │  │ Store/Svc> │  │  _task        │
    │  WireGuard>    │  │ Corrosion  │  │ tasks/peer_   │
    └────────────────┘  │ Store      │  │ sync.rs       │
                        └────────────┘  └───────┬───────┘
                                                │ owns
                                        ┌───────▼───────┐
                                        │ PeerStateMap  │
                                        │ mesh/peer_    │
                                        │ state.rs      │
                                        └───────────────┘

    ┌─────────────┐       mpsc(32)       ┌─────────────┐
    │ Unix Socket │─────────────────────►│ Daemon Loop │
    │  Listener   │   IncomingCommand    │  (ployzd)   │
    │ transport/  │◄─────────────────────│             │
    │ listener.rs │   oneshot<Response>  └─────────────┘
    └─────────────┘
```

---

## 7. Arc<Mutex<T>> Candidates for Actor-Owned State

These are cases where an `Arc<Mutex<T>>` (or `Arc<T>` with internal Mutex) could potentially be replaced by actor-owned state if the system moved to an actor model:

| # | Current Type | File | Candidate? | Rationale |
|---|-------------|------|-----------|-----------|
| 1 | `Arc<MemoryStore>` + `Mutex<StoreInner>` | `adapters/memory/store.rs:9-10` | **Strong candidate** | Only two accessors: daemon loop and peer_sync task. Could become an actor that processes store commands via a channel, eliminating the Mutex entirely. Subscriber broadcast already uses channels. |
| 2 | `Arc<MemoryWireGuard>` + `Mutex<WgInner>` | `adapters/memory/wireguard.rs:6-7` | **Strong candidate** | Mock-only, but same pattern. Could be actor-owned with command messages for up/down/set_peers. |
| 3 | `Arc<HostWireGuard>` + `Mutex<WgBackend>` | `adapters/wireguard/host.rs:24-25` | **Moderate candidate** | Genuine concurrency but low contention. An actor would serialize WG API calls naturally. However, the underlying WireGuard API calls are fast and the Mutex is appropriate as-is. |
| 4 | `Arc<DockerWireGuard>` | `adapters/wireguard/docker.rs:23` | **Weak candidate** | No internal Mutex — stateless Docker CLI bridge. Arc is only for Clone semantics. |
| 5 | `Arc<DockerCorrosion>` | `adapters/corrosion/docker.rs:12` | **Weak candidate** | Same — no internal mutable state to protect. |
| 6 | `Arc<MemoryService>` | `adapters/memory/service.rs:5` | **Weak candidate** | Mock service, no meaningful state. |

### Recommendation Priority

1. **MemoryStore** is the strongest actor candidate — it already has a subscription/broadcast pattern that is naturally actor-shaped. Converting it to a channel-based actor would eliminate the `std::sync::Mutex`, remove the `Arc`, and make the subscriber broadcast non-blocking.

2. **HostWireGuard** is worth considering if WG operations ever become slow (e.g., many peers). Serializing through an actor would provide natural backpressure.

3. The `Docker*` and `MemoryService` wrappers are thin enough that actor conversion would add complexity for no gain.

---

## 8. Ambiguous or Contested Ownership

### 8.1 `Mesh::store()` returns a clone — ownership leak

**File:** `src/mesh/orchestrator.rs:60-62`

```rust
pub fn store(&self) -> StoreDriver {
    self.store.clone()
}
```

This method hands out cheap clones of the `StoreDriver` to callers. The daemon handlers use this extensively (`handlers.rs:304,310,334,343,366,476,522`). This means the `StoreDriver` (and its inner `Arc` pointers) are accessible from:
- The `Mesh` itself (lifecycle operations)
- The `peer_sync` spawned task (via clone at `orchestrator.rs:126`)
- Every handler that calls `mesh.store()` (via `DaemonState`)

**Risk:** After `Mesh::destroy()` tears down the service, a previously-cloned `StoreDriver` could still attempt operations. The code does handle this carefully (e.g., `handle_mesh_down` calls `destroy()` then uses the pre-cloned store for cleanup at `handlers.rs:304-310`), but this is a pattern where a leaked clone could outlive the service.

### 8.2 `handle_mesh_down` — store outlives mesh

**File:** `src/daemon/handlers.rs:299-315`

```rust
let store = active.mesh.store();     // clone store
active.mesh.destroy().await?;         // destroy mesh (stops service)
store.delete_machine(...).await?;     // use store AFTER destroy
```

This is intentional — the local machine record must be deleted after the mesh is torn down. But it means the store's Arc keeps the underlying adapter alive after `ServiceControl::stop()`. The Corrosion HTTP client will fail gracefully; the Memory mock will succeed silently. This is a design choice, not a bug, but it's worth noting.

### 8.3 `StoreDriver::clone()` is implicit via `#[derive(Clone)]`

**File:** `src/backends.rs:53`

Both `WireguardDriver` and `StoreDriver` derive `Clone`. This makes it very easy to proliferate shared references without explicit `Arc::clone()` calls at the usage site. The cloning is hidden inside the enum's derived `Clone` impl.

**Risk:** Makes it harder to audit who holds references. A `grep` for `Arc::clone` will miss these.

### 8.4 Corrosion subscription bridge task — orphan risk

**File:** `src/adapters/corrosion/mod.rs:177-195`

When `subscribe_machines()` is called on `CorrosionStore`, it spawns a background task that bridges the Corrosion streaming API into an mpsc channel. This task is **not tracked** by `TaskSet` — it's a bare `tokio::spawn`. It will exit when the receiver is dropped (line 185: `if tx.send(ev).await.is_err() { return; }`), but there's no explicit cancellation mechanism.

**Risk:** If the receiver is held open but never consumed, this task will accumulate events until the channel is full, then block on `send`. Unlikely in practice (the receiver is consumed by `peer_sync` which has its own cancellation), but the task lifecycle is not explicitly managed.

---

## 9. Summary Table

| Component | Owner | Shared Via | Concurrency | Clean? |
|-----------|-------|-----------|-------------|--------|
| `DaemonState` | daemon loop | Not shared | Single-threaded | Yes |
| `ActiveMesh` | DaemonState | Not shared | Single-threaded | Yes |
| `Mesh` | ActiveMesh | Not shared | Single-threaded | Yes |
| `WireguardDriver` | Mesh | `Clone` (Arc inside) | Daemon + peer_sync | Yes |
| `StoreDriver` | Mesh | `Clone` (Arc inside) | Daemon + peer_sync | Mostly (see 8.1) |
| `TaskSet` | Mesh | Not shared | Owns spawned tasks | Yes |
| `PeerStateMap` | peer_sync task | Not shared | Task-local | Yes |
| `Identity` | DaemonState | Field clones | Read-only | Yes |
| `NetworkConfig` | ActiveMesh | Value clones | Read-only | Yes |
| `MemoryStore` internals | Mutex | `Arc<MemoryStore>` | Low contention | Yes |
| `HostWireGuard` backend | Mutex | `Arc<HostWireGuard>` | Genuine | Yes |
| Corrosion bridge task | tokio runtime | Not tracked | Orphan-possible | See 8.4 |

---

## 10. Key Observations

1. **The codebase has clean top-down ownership.** `DaemonState` → `ActiveMesh` → `Mesh` → backends/tasks. No circular references, no global state, no lazy_static.

2. **Arc usage is minimal and justified.** All Arc wrapping serves the same purpose: enabling `Clone` on `WireguardDriver`/`StoreDriver` enums so they can be passed into `tokio::spawn` boundaries.

3. **Mutex usage is conservative.** Only 3 instances, all using `std::sync::Mutex` (not tokio), all with short critical sections that never cross await points.

4. **Channel architecture is well-layered.** Command dispatch (mpsc + oneshot), event subscription (mpsc per subscriber), and task cancellation (CancellationToken) are cleanly separated.

5. **The main risk area is store clone proliferation** (§8.1). The `Mesh::store()` accessor freely hands out clones, making it possible for store references to outlive the service. Currently handled correctly but fragile under future modification.
