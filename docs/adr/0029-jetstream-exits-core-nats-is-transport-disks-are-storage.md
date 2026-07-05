# JetStream Exits: Core NATS Is Transport, Disks Are Storage

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
  Operation history is mortal with the core, by declaration (ADR 0001's
  disposable class); Cloud subscribes live if it wants durable memory.
- **KV locks and terminal-event dedup** → the core sequencer's in-process
  mutexes and its own write-once records. Distributed locks existed for
  independent writers sharing no process; after ADR 0028 there are none.
- **Durable job triggers and schedules** → RPC that fails loudly plus
  operator retry; local timers that create operations (cert renewal).
- **Object store** → RPC push of artifacts to the machines that use them.

Consequences: "Reindex" stops being a concept — after a total JetStream or
core-disk wipe, intent re-adopts from evidence files and facts repopulate
within one broadcast tick, so recovery is `assure` plus 30 seconds. The
JetStream provisioning, assurance, and per-test bootstrap surface (and its
contention flake class) is deleted rather than maintained. The cost,
stated plainly: no durable shared history survives the core, cold cluster
reads are a bounded gather or a cache marked with observed-at, and an
offline machine is visible only as last-known-good evidence wherever some
reader cached it.
