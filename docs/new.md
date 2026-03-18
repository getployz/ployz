Refactor Principles
- Prefer fewer crates with sharper roles over many thin-but-confusing wrappers.
- Delete compatibility shims instead of carrying them.
- Move behavior to the layer where it is easiest to understand, not where it was historically placed.
- If a crate mostly adapts one crate to another, try to remove it.
- If a module needs a long comment to explain why it exists, the structure is probably wrong.
Target End State
- ployz-types - pure domain model/spec/value types only
- ployz-store-api - the only durable-store seam
- ployz-runtime-api - the only runtime/network/dataplane seam
- ployz-orchestrator - the orchestration kernel: deploy/apply, mesh lifecycle, join/bootstrap, reconciliation, liveness, IPAM
- ployz-corrosion - concrete Corrosion backend implementing ployz-store-api
- ployz-runtime-backends - concrete Docker/WireGuard/eBPF/runtime implementations of ployz-runtime-api
- ployz-api - daemon protocol only
- ployz-sdk - future-facing client facade over ployz-api, plus curated model/protocol exports
- ployz-config - config loading/defaults/path resolution only
- ployzd - composition root and CLI only
- ployz-gateway / ployz-dns - thin edge services over abstract routing-store input
- ployz-e2e - test harness over typed daemon interactions
- ebpf-common / ebpf / ployz-bpfctl - one coherent dataplane slice
Crates To Delete Or Collapse
- Delete ployz-store-corrosion if possible; it is mostly an extra assembly layer.
- Delete ployz-state as a crate if possible; its pure pieces should move either:
  - into ployz-orchestrator if they are orchestration policy
  - into ployz-types if they are pure model/value logic
  - into ployz-runtime-backends if they are concrete host/network/dataplane behavior
  - into ployz-config if they are config/path concerns
That is the biggest readability win in the whole repo.
What Goes Where
- Move from ployz-state to ployz-orchestrator
  - invites/join flows
  - machine liveness
  - IPAM
  - bootstrap/domain state coordination
- Move from ployz-state to ployz-runtime-backends
  - endpoint probing
  - eBPF attach/control
  - Docker/container/network execution
  - host interface inspection
- Move from ployz-state to ployz-config
  - any local filesystem/path/config persistence helpers
- Move from ployz-runtime-backends to ployz-orchestrator
  - deploy preview/apply orchestration
  - rollout/participant/cleanup policy
- Move from ployz-api to ployz-sdk
  - stdio/unix transports
  - typed client helpers
- Move out of ployz-config
  - affordance probing
  - runtime validation policy
SDK Direction
Keep ployz-sdk, but make it intentional:
- expose a typed client
- expose connection config for consumers
- re-export stable domain/protocol types consumers actually need
- own concrete client transports
- do not expose store traits, runtime traits, daemon-local config, path helpers, or host probing
In short:
- ployz-api = protocol
- ployz-sdk = consumer experience
Refactor Waves
Wave 1: Delete Duplicate Seams
- Pick one store seam: ployz-store-api
- Move the useful traits from crates/ployz-types/src/store.rs:8 into crates/ployz-store-api/src/lib.rs:12
- Remove store lifecycle methods from the store seam
- Replace Tokio channel types in public store APIs with a crate-owned event/subscription abstraction
- Reduce ployz-types to pure models only
- Outcome: contracts become readable and dependency flow becomes obvious
Wave 2: Eliminate ployz-state
- Move pure orchestration logic into ployz-orchestrator
- Move concrete runtime/network behavior into ployz-runtime-backends
- Move config/path helpers into ployz-config
- Update imports repo-wide
- Delete ployz-state
- Outcome: one whole confusing middle layer disappears
Wave 3: Clean Runtime And Store Boundaries
- Rewrite ployz-runtime-api so it depends on no concrete orchestrator/state types
- Make ployz-corrosion implement ployz-store-api directly
- Remove ployz-store-corrosion, or reduce it to a temporary constructor and then delete it
- Outcome: backend crates stop pointing back up the stack
Wave 4: Move Kernel Policy Home
- Move deploy/apply logic from crates/ployz-runtime-backends/src/deploy/mod.rs:31 into ployz-orchestrator
- Split crates/ployz-orchestrator/src/mesh/orchestrator.rs:45 into smaller modules:
  - lifecycle
  - bootstrap/join
  - peer state
  - task runtime
  - deploy coordination
- Outcome: orchestration policy lives in one place
Wave 5: Redesign API + SDK
- Make ployz-api protocol-only
- Move transports into ployz-sdk
- Build a small typed SDK client instead of raw enum-centric access
- Prune ployz-sdk so it is curated, not a barrel file
- Outcome: future-facing API surface becomes clear now, not later
Wave 6: Thin The Edge Crates
- Break up crates/ployzd/src/main.rs:462
- Make ployzd mostly:
  - parse args
  - build dependencies
  - dispatch commands
- Remove direct backend-specific logic from ployzd where possible
- Make ployz-gateway and ployz-dns take abstract routing-store inputs instead of constructing Corrosion-backed stores themselves
- Outcome: binaries become easy to scan and reason about
Wave 7: Simplify The Dataplane Slice
- Extract shared userspace eBPF control logic used by:
  - crates/ployz-bpfctl/src/linux.rs:77
  - crates/ployz-state/src/network/ebpf/native.rs:29 or its moved replacement
- Bring ebpf under workspace governance if practical
- Remove duplicated attach/map/pin path logic
- Outcome: one dataplane implementation model, not two
Wave 8: Clean Tests And Harness
- Split crates/ployz-e2e/src/runner.rs:36
- Stop parsing human CLI output where possible
- Add typed daemon-facing test helpers
- Outcome: refactor stays testable and future churn gets cheaper
How I’d Execute It In Practice
- Use one dedicated refactor branch
- Land this in large, coherent waves, not tiny compatibility-preserving PRs
- Allow internal API breakage inside the branch
- Keep merge checkpoints green at the end of each wave
- Do not spend time on temporary abstractions unless they reduce risk materially
Readability Rules For The Refactor
- No “util” or “common” modules unless the name is domain-specific
- No module should mix pure logic and side effects
- No crate root should re-export everything by default
- Prefer 5 small files over 1 giant coordinator file
- Prefer explicit types and constructors over “manager” buckets
- Prefer one trait per seam family over mega-traits
- Prefer deleting dead/half-implemented surfaces over preserving them
Likely Final Crate Set
- keep: ployz-config, ployz-api, ployz-types, ployz-store-api, ployz-runtime-api, ployz-orchestrator, ployz-sdk, ployz-runtime-backends, ployz-corrosion, ployzd, ployz-gateway, ployz-dns, ployz-e2e, ebpf-common, ployz-bpfctl, ebpf
- remove: ployz-state, ployz-store-corrosion
Definition Of Done
- ployz-state is gone
- ployz-store-corrosion is gone
- ployz-types is pure
- ployz-api is protocol-only
- ployz-sdk is clean and future-facing
- ployz-orchestrator owns orchestration policy
- ployz-runtime-backends only implements concrete backends
- ployzd, ployz-gateway, and ployz-dns are thin
- dependency direction is obvious from Cargo.toml alone
- hotspot files have been split into readable modules
