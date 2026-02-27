Ruthless Tree: Rebuild internal/ from scratch

 Context

 Ployz's internal/ has grown organically into 15+ packages with tangled boundaries. The network/ package alone holds domain types, infrastructure lifecycle, membership CRUD,
 and peer reconciliation. Convergence is scattered across engine/, supervisor/, watch/, and health/*. Deploy has never successfully run end-to-end.

 Goal: Rebuild internal/ as internal/daemon/ with four capabilities, one startup script, one orchestrator, and one wiring root. Everything fails loudly. Phases on everything
 so agents can reason about state.

 Design principles:
 - Agent-first: every operation returns a phase, every error is structured and actionable
 - Loud failure: succeed completely or return a rich error with component, phase, reason, hint
 - One startup story: daemon/run.go boots everything, daemon/wiring.go wires dependencies
 - One orchestrator: daemon/manager.go — cross-capability policy lives here, not in transport
 - Four capabilities: overlay, membership, convergence, workload (stubbed)
 - Narrow dependency interfaces: capabilities depend on small ports, not on each other's Service structs
 - Consistent package skeleton: model.go, ports.go, service.go, errors.go

 Execution strategy: Sequence by behavior-critical boundaries, not by folder rename. Create new packages, move logic, repoint consumers, then rename/regroup at the end.

 ---
 Target Tree

 internal/
   daemon/
     run.go                        # boot/shutdown script
     wiring.go                     # single composition root
     manager/                      # orchestrator: cross-capability policy, API impl
       manager.go                  # Manager struct, constructor, options
       network_ops.go              # ApplyNetworkSpec, DisableNetwork
       machine_ops.go              # ListMachines, UpsertMachine, RemoveMachine
       status_ops.go               # GetStatus, GetIdentity, GetPeerHealth
       workload_ops.go             # PlanDeploy, ApplyDeploy, etc. (stubbed)
       ports.go                    # SpecStore, PersistedSpec
       errors.go                   # Manager-level sentinel errors
     overlay/                      # one-shot infra lifecycle
       model.go                    # Config, State, CorrosionConfig, NetworkRuntimePhase
       ports.go                    # PlatformOps, CorrosionRuntime, StatusProber, StateStore, Clock, ContainerRuntime
       service.go                  # Service struct + Start/Stop/Status/Resolve + Option funcs
       start.go                    # startRuntime logic
       stop.go                     # stopRuntime logic
       status.go                   # Status + buildRuntimeTree
       errors.go                   # ErrNetworkNotConfigured, etc.
     membership/                   # machine/heartbeat CRUD + subscriptions
       model.go                    # MachineRow, HeartbeatRow, ChangeKind, MachineChange, HeartbeatChange, Peer, PeerSpec
       ports.go                    # StateReader, PeerApplier, MachineRegistry, HeartbeatRegistry, Registry, RegistryFactory, NetworkConfigRegistry
       service.go                  # Service struct + Reconcile, ReconcilePeers, ListMachines, Upsert, Remove
       machines.go                 # machine operations
       heartbeats.go               # heartbeat operations
       subscribe.go                # subscription types/helpers
       errors.go                   # ErrConflict, membership-specific errors
     convergence/                  # continuous peer sync + health
       model.go                    # NetworkHealth, PeerHealth, SupervisorPhase, LoopPhase
       ports.go                    # PeerReconciler (slim: just ReconcilePeers)
       service.go                  # Service struct (replaces Engine + Supervisor)
       loop.go                     # the event loop
       broker.go                   # subscription multiplexer
       reconcile.go                # peer reconciliation helpers
       heartbeat.go                # heartbeat writer goroutine
       freshness.go                # peer freshness tracking
       ping.go                     # TCP ping tracking
       ntp.go                      # NTP clock checking
       errors.go
     workload/                     # STUBBED — deploy rebuilt later
       model.go                    # minimal: DeployPhase, DeployError, ProgressEvent
       ports.go                    # interfaces: ContainerStore, DeploymentStore, HealthChecker
       service.go                  # Service struct, all methods return "not yet implemented"
       errors.go                   # DeployErrorPhase, DeployErrorReason enums
     api/                          # gRPC transport only
       server.go                   # ListenAndServe, socket lifecycle
       handlers.go                 # thin gRPC → manager delegation
       mapping.go                  # proto ↔ types conversion
       errors.go                   # gRPC status mapping
     proxy/                        # multi-machine routing
       director.go
       backend.go
       local.go
       remote.go
     pb/                           # generated protobuf
   infra/                          # external integrations (renamed from adapter/ last)
     corrosion/
     docker/
     wireguard/
     sqlite/
     platform/
   support/                        # cross-cutting (renamed/regrouped last)
     check/
     logging/
     buildinfo/
     remote/

 ---
 Key Design Decisions

 Manager as orchestrator (not handlers calling services directly)

 daemon/manager/ is a sub-package with one Manager type split across multiple files by concern — same pattern as today's controlplane/manager/. It implements the client.API
 interface. Handlers call manager, manager calls capabilities. Cross-capability policy lives in the manager, not in transport.

 daemon/manager/
   manager.go          # Manager struct, constructor, options
   network_ops.go      # ApplyNetworkSpec, DisableNetwork
   machine_ops.go      # ListMachines, UpsertMachine, RemoveMachine
   status_ops.go       # GetStatus, GetIdentity, GetPeerHealth
   workload_ops.go     # PlanDeploy, ApplyDeploy, etc. (stubbed)
   ports.go            # SpecStore, PersistedSpec
   errors.go           # Manager-level sentinel errors

 Narrow dependency interfaces for membership

 Membership does NOT depend on overlay.Service. Instead it depends on narrow ports:

 // membership/ports.go
 type StateReader interface {
     LoadState(dataDir string) (overlay.State, error)
 }

 type PeerApplier interface {
     ApplyPeerConfig(ctx context.Context, cfg overlay.Config, state overlay.State, peers []Peer) error
 }

 These are implemented by overlay.Service but membership only sees the narrow interface. This prevents coupling and cycles.

 Feature regression is intentional and explicit

 Deploy is stubbed. The CLI and API error mapping must make this clear:
 - API: deploy RPCs return codes.Unimplemented with message "deploy is being rebuilt — not yet available"
 - CLI: ployz service deploy/run/list/status/remove print the same message and exit 1

 ---
 Execution Plan

 Step 1: Create daemon/ skeleton and capability packages

 Create all directories and empty package files. No logic moves yet. Just establish the new package structure so subsequent steps can move code into real targets.

 Create:
 - internal/daemon/{run,wiring,manager}.go — package stubs
 - internal/daemon/overlay/{model,ports,service,start,stop,status,errors}.go
 - internal/daemon/membership/{model,ports,service,machines,heartbeats,subscribe,errors}.go
 - internal/daemon/convergence/{model,ports,service,loop,broker,reconcile,heartbeat,freshness,ping,ntp,errors}.go
 - internal/daemon/workload/{model,ports,service,errors}.go

 Step 2: Collapse convergence (engine + supervisor + watch + health → one package)

 This is the cleanest first move — these packages are only consumed by the manager, and collapsing them doesn't change any external interfaces.

 Source → Destination:

 ┌────────────────────────────────────────────────────────────────────────┬─────────────────────────────────────────────────┐
 │                                 Source                                 │                   Destination                   │
 ├────────────────────────────────────────────────────────────────────────┼─────────────────────────────────────────────────┤
 │ engine/engine.go (Engine, Start/Stop/Status/Health, runSupervisorLoop) │ convergence/service.go                          │
 ├────────────────────────────────────────────────────────────────────────┼─────────────────────────────────────────────────┤
 │ engine/phase.go (SupervisorPhase)                                      │ convergence/model.go                            │
 ├────────────────────────────────────────────────────────────────────────┼─────────────────────────────────────────────────┤
 │ engine/ports.go (factories)                                            │ DELETE — no more factory indirection            │
 ├────────────────────────────────────────────────────────────────────────┼─────────────────────────────────────────────────┤
 │ supervisor/supervisor.go (Supervisor, Run)                             │ convergence/loop.go                             │
 ├────────────────────────────────────────────────────────────────────────┼─────────────────────────────────────────────────┤
 │ supervisor/phase.go (LoopPhase)                                        │ convergence/model.go                            │
 ├────────────────────────────────────────────────────────────────────────┼─────────────────────────────────────────────────┤
 │ supervisor/ports.go                                                    │ convergence/ports.go (PeerReconciler interface) │
 ├────────────────────────────────────────────────────────────────────────┼─────────────────────────────────────────────────┤
 │ watch/broker.go + watch/topic.go                                       │ convergence/broker.go                           │
 ├────────────────────────────────────────────────────────────────────────┼─────────────────────────────────────────────────┤
 │ health/freshness/tracker.go                                            │ convergence/freshness.go                        │
 ├────────────────────────────────────────────────────────────────────────┼─────────────────────────────────────────────────┤
 │ health/ping/tracker.go                                                 │ convergence/ping.go                             │
 ├────────────────────────────────────────────────────────────────────────┼─────────────────────────────────────────────────┤
 │ health/ntp/checker.go                                                  │ convergence/ntp.go                              │
 └────────────────────────────────────────────────────────────────────────┴─────────────────────────────────────────────────┘

 Key changes:
 - No more factory interfaces. Service receives already-built deps at construction.
 - convergence.Service replaces both Engine and Supervisor as a single type.
 - Start() runs the retry/backoff loop in a goroutine (from engine.runSupervisorLoop).
 - loop.go contains the select-based event loop (from supervisor.Run).
 - Freshness/Ping/NTP trackers become internal to the package, not separate packages.
 - convergence.Service still depends on network.Config, supervisor.PeerReconciler, etc. via interfaces — we'll update these imports in step 3.

 Step 3: Split network/ into overlay/ + membership/

 The big split. network/ currently holds 3 concerns in one package. We separate them.

 overlay/ gets:

 ┌────────────────────────────────────────────────────────────────────────────────────────────────────────────┬───────────────────────────────┬────────────────────────────┐
 │                                                   Source                                                   │          Destination          │           Notes            │
 ├────────────────────────────────────────────────────────────────────────────────────────────────────────────┼───────────────────────────────┼────────────────────────────┤
 │ network/config.go (Config, NormalizeConfig, ConfigFromSpec)                                                │ overlay/model.go              │                            │
 ├────────────────────────────────────────────────────────────────────────────────────────────────────────────┼───────────────────────────────┼────────────────────────────┤
 │ network/state.go (State, ensureState)                                                                      │ overlay/model.go              │                            │
 ├────────────────────────────────────────────────────────────────────────────────────────────────────────────┼───────────────────────────────┼────────────────────────────┤
 │ network/phase.go (NetworkRuntimePhase)                                                                     │ overlay/model.go              │                            │
 ├────────────────────────────────────────────────────────────────────────────────────────────────────────────┼───────────────────────────────┼────────────────────────────┤
 │ network/resolved.go (Resolve)                                                                              │ overlay/model.go              │                            │
 ├────────────────────────────────────────────────────────────────────────────────────────────────────────────┼───────────────────────────────┼────────────────────────────┤
 │ network/ports.go (Clock, ContainerRuntime, CorrosionRuntime, StatusProber, PlatformOps, StateStore,        │ overlay/ports.go              │                            │
 │ CorrosionConfig + all supporting types)                                                                    │                               │                            │
 ├────────────────────────────────────────────────────────────────────────────────────────────────────────────┼───────────────────────────────┼────────────────────────────┤
 │ network/runtime_common.go (startRuntime, stopRuntime)                                                      │ overlay/start.go +            │                            │
 │                                                                                                            │ overlay/stop.go               │                            │
 ├────────────────────────────────────────────────────────────────────────────────────────────────────────────┼───────────────────────────────┼────────────────────────────┤
 │ network/controller.go (Controller struct, Options, New)                                                    │ overlay/service.go            │ Rename Controller →        │
 │                                                                                                            │                               │ Service                    │
 ├────────────────────────────────────────────────────────────────────────────────────────────────────────────┼───────────────────────────────┼────────────────────────────┤
 │ network/status.go                                                                                          │ overlay/status.go             │                            │
 ├────────────────────────────────────────────────────────────────────────────────────────────────────────────┼───────────────────────────────┼────────────────────────────┤
 │ network/errors.go                                                                                          │ overlay/errors.go             │                            │
 ├────────────────────────────────────────────────────────────────────────────────────────────────────────────┼───────────────────────────────┼────────────────────────────┤
 │ network/management.go                                                                                      │ overlay/model.go              │ ManagementIPFromPublicKey  │
 ├────────────────────────────────────────────────────────────────────────────────────────────────────────────┼───────────────────────────────┼────────────────────────────┤
 │ network/bootstrap.go                                                                                       │ overlay/model.go              │                            │
 ├────────────────────────────────────────────────────────────────────────────────────────────────────────────┼───────────────────────────────┼────────────────────────────┤
 │ network/docker_network.go                                                                                  │ overlay/model.go              │ Bridge name helpers        │
 └────────────────────────────────────────────────────────────────────────────────────────────────────────────┴───────────────────────────────┴────────────────────────────┘

 membership/ gets:

 ┌──────────────────────────────────────────────────────────────────────────────────────┬────────────────────────┬───────┐
 │                                        Source                                        │      Destination       │ Notes │
 ├──────────────────────────────────────────────────────────────────────────────────────┼────────────────────────┼───────┤
 │ network/machine.go (MachineRow, HeartbeatRow, ChangeKind, etc.)                      │ membership/model.go    │       │
 ├──────────────────────────────────────────────────────────────────────────────────────┼────────────────────────┼───────┤
 │ network/types.go (Machine)                                                           │ membership/model.go    │       │
 ├──────────────────────────────────────────────────────────────────────────────────────┼────────────────────────┼───────┤
 │ network/peers.go (Peer, PeerSpec, BuildPeerSpecs, etc.)                              │ membership/model.go    │       │
 ├──────────────────────────────────────────────────────────────────────────────────────┼────────────────────────┼───────┤
 │ network/controlplane.go (Reconcile, ReconcilePeers, ListMachines, Upsert, Remove)    │ membership/machines.go │       │
 ├──────────────────────────────────────────────────────────────────────────────────────┼────────────────────────┼───────┤
 │ network/ports.go (MachineRegistry, NetworkConfigRegistry, Registry, RegistryFactory) │ membership/ports.go    │       │
 └──────────────────────────────────────────────────────────────────────────────────────┴────────────────────────┴───────┘

 membership/ports.go narrow interfaces:
 type StateReader interface {
     LoadState(dataDir string) (overlay.State, error)
 }
 type PeerApplier interface {
     ApplyPeerConfig(ctx context.Context, cfg overlay.Config, state overlay.State, peers []Peer) error
 }

 overlay.Service implements both, but membership only imports the interfaces.

 Step 4: Repoint manager to new capability services

 Replace controlplane/manager/ internals to use the new packages. The manager struct changes from holding *network.Controller + *engine.Engine to holding *overlay.Service +
 *membership.Service + *convergence.Service + *workload.Service.

 Files to update:
 - controlplane/manager/manager_types.go — new Manager struct fields
 - controlplane/manager/manager_construct.go — New() accepts new service types
 - controlplane/manager/manager_production.go — wiring uses new packages
 - controlplane/manager/manager_network_ops.go — calls overlay.Service + convergence.Service
 - controlplane/manager/manager_status_ops.go — calls overlay + convergence for status/health
 - controlplane/manager/manager_machine_ops.go — calls membership.Service
 - controlplane/manager/manager_spec_resolution.go — uses overlay.Config

 This step keeps the manager in its current location (controlplane/manager/). We move it in step 5.

 Step 5: Move controlplane/ → daemon/

 Move the remaining controlplane pieces into their daemon/ homes.

 Moves:
 - controlplane/manager/* → daemon/manager/* (same multi-file split, new import path)
 - controlplane/api/* → daemon/api/*
 - controlplane/proxy/* → daemon/proxy/*
 - controlplane/pb/* → daemon/pb/*

 Key changes:
 - Manager becomes daemon/manager.Manager — same multi-file pattern as today, split by concern
 - API handlers delegate to daemon.Manager, not directly to capabilities
 - Proto import path changes from ployz/internal/controlplane/pb to ployz/internal/daemon/pb
 - Regenerate proto if go_package option needs updating

 Step 6: Stub workload/ and remove old deploy path

 Intentional feature regression. Deploy RPCs return Unimplemented.

 Create workload/ stubs:
 - workload/model.go — DeployPhase, DeployError, ProgressEvent types
 - workload/ports.go — ContainerStore, DeploymentStore, HealthChecker, StateReader interfaces
 - workload/service.go — all methods return fmt.Errorf("workload: not yet implemented")
 - workload/errors.go — DeployErrorPhase, DeployErrorReason enums

 Delete:
 - internal/deploy/ — all files
 - internal/observed/ — all files (only served deploy caching)
 - Deploy-related logic from manager (the old manager_deploy_ops.go content)

 Update daemon/api/handlers.go:
 - Deploy RPCs return status.Error(codes.Unimplemented, "deploy is being rebuilt — not yet available")

 Update CLI commands:
 - cmd/ployz/service/deploy.go, run.go, list.go, status.go, remove.go — print clear message and exit 1

 Step 7: Rename adapter/ → infra/, regroup support/

 Pure renames, done last to minimize diff noise.

 Moves:
 - internal/adapter/* → internal/infra/*
 - internal/check/ → internal/support/check/
 - internal/logging/ → internal/support/logging/
 - internal/buildinfo/ → internal/support/buildinfo/
 - internal/remote/ → internal/support/remote/

 Fix all import paths. just build must pass.

 Step 8: Delete old packages

 Remove everything replaced:
 - internal/network/
 - internal/engine/
 - internal/supervisor/
 - internal/watch/
 - internal/health/
 - internal/controlplane/

 ---
 Import Dependency Order (no cycles)

 infra/* → stdlib + external only
 support/* → stdlib only
 daemon/overlay/ → infra/*, support/check
 daemon/membership/ → overlay (types only: Config, State), infra/*
 daemon/convergence/ → membership (PeerReconciler interface), overlay (StateStore, Clock types)
 daemon/workload/ → (stubbed, minimal deps)
 daemon/manager/ → overlay, membership, convergence, workload
 daemon/api/ → daemon/manager/, daemon/pb
 daemon/proxy/ → daemon/pb
 daemon/ (run.go, wiring.go) → all of the above

 ---
 Verification

 After all steps:
 1. just build && just test — clean
 2. No stale imports: grep -rn '"ployz/internal/network"' --include='*.go' . returns nothing (same for engine, supervisor, deploy, controlplane, watch, health, observed)
 3. Smoke test: start daemon → ployz network create default → ployz node status → ployz network destroy default
 4. Deploy commands return clear "not yet implemented" errors with exit code 1

 ---
 What We're NOT Doing (Yet)

 - Building the deploy pipeline — workload/ is stubbed; rebuilt fresh in a follow-up
 - Moving deploy orchestration to SDK — decide after workload/ is working
 - Changing the proto RPC surface — same RPCs, deploy ones return Unimplemented
 - Adding new features — this is restructure + cleanup
