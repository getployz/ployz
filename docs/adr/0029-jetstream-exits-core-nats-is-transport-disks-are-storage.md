# JetStream Exits: Core NATS Is Transport, Disks Are Storage

**Superseded by [ADR 0040](0040-corrosion-replaces-the-core-and-nats.md).**

This replaces the former JetStream classification and reindex model for
control-plane storage.

With state split per ADR 0028, every JetStream feature the control plane
uses has a simpler owner, so JetStream is removed entirely
(`jetstream: disabled`): the disabled config is the enforcement — there is
no bucket to quietly put new shared state in. Nothing in the cluster runs
consensus, including the replicated-stream kind wearing a NATS costume.

The replacement for each tenant:

- **KV current-state buckets** → fact broadcasts and intent files with
  periodic rebroadcast (ADR 0028). KV watch → subscribe plus re-list on
  reconnect.
- **Operation events and status stores** → the sequencer's local
  append-only evidence log (one file per operation, age/count capped,
  deleted by `rm`). Live progress rides plain subjects; `ops watch`
  replays the file then tails; `ops` queries name any machine that did not
  answer instead of presenting partial results as complete. Machines keep
  their own step evidence in their fact ledgers, so a deep inspection can
  corroborate an operation from the machines it touched — everyone
  testifies about themselves, nobody holds another party's memory.
  Operation history is mortal with the core by declaration; Cloud subscribes
  live if it wants durable memory.
  Standalone Ployz does not promise durable long-term operation history
  after core evidence loss. Evidence logs live in ployzd-owned `0700`
  directories as `0600` files, are written atomically with fsync, exclude
  secret values, redact credentials, tokens, and cert material, and are
  removed by the same retention deletion that removes archived evidence.
  One scoped exception: the sequencer's mutable working index may hold an
  in-flight operation's own secret material — a join token, a minted seed —
  in cleartext at `0600` for exactly as long as idempotent resume needs it,
  and scrubs it when the operation reaches a terminal state. This is the
  minimum window that keeps resume working without a key-management
  dependency; the durable, shareable append-only event log never carries a
  secret. Restricting (`0600`) is the boundary for this transient index;
  exclude/redact remains the rule for the durable log.
- **KV locks and terminal-event dedup** → the core sequencer's in-process
  mutexes and its own write-once records. Distributed locks existed for
  independent writers sharing no process; after ADR 0028 there are none.
- **Durable job triggers and schedules** → RPC that fails loudly plus
  operator retry; local timers that create operations (cert renewal).
  Before schedules disappear, each former durable trigger is classified as
  recreated from intent, failed into a visible operation, or deliberately
  operator-owned. ACME renewal is recreated by a core-owned local timer that
  emits operation evidence on missed or failed renewal work.
- **Object store** → RPC push of artifacts to the machines that use them.
  Each pushed artifact carries expected digest, size, type, and operation id;
  machines verify those fields before writing or using the artifact and
  report a typed operation failure on mismatch.

Consequences: "Reindex" stops being a concept. After JetStream loss or core
restart with preserved or adopted intent evidence files, recovery is `assure`
plus one broadcast tick. After core evidence loss, operator intent is gone
unless restored from an external or operator-held source; machine facts can
repopulate runtime views, but not the lost intent. The JetStream provisioning,
assurance, and per-test bootstrap surface (and its contention flake class) is
deleted rather than maintained. The cost, stated plainly: no durable shared
history survives the core, cold cluster reads are a bounded gather or a cache
marked with observed-at, and an offline machine is visible only as
last-known-good evidence wherever some reader cached it.
