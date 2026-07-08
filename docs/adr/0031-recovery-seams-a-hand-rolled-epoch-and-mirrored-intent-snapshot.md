# Recovery Seams: A Hand-Rolled Control-Plane Epoch And A Mirrored Intent Snapshot

ADR 0030 decided *how the fleet finds a new core* — operator-promoted, epoch-gated,
pull-reconnecting, with intent mirrored off the drumbeat. It did not pin the
mechanisms, and the obvious question when building them is: **we run hard on
NATS — what does NATS give us, and what do we build ourselves?** This ADR answers
that, fixes the concrete seams, and orders them, because the dependencies between
them are load-bearing (the candidate list is useless before the mirror exists).

The decision, seam by seam:

- **The Control-Plane Epoch is a hand-rolled monotonic `u64` carried on the intent
  drumbeat — not a NATS primitive.** The core persists it and advertises it on
  `intent.changed` (and echoes it in the `intent.get` reply); a machine persists
  the highest epoch it has seen and rejects a lower one, which is what fences it
  away from a healed old core. The old core is repaired by an explicit core
  replacement command after the operator has resolved the partition. NATS's only
  epoch-shaped primitives — JetStream KV revision,
  stream sequence, the meta-group's Raft term — all live in the JetStream we
  exited (ADR 0029), and leaning on them re-imports the exact consensus the
  disposable-core model exists to avoid (ADR 0030). So the epoch is ours: a
  counter on a plain subject, which is already the fanout model. NATS carries the
  value; it does not define it.

- **Intent is mirrored as the `IntentSnapshot`, persisted machine-side.**
  The payload is `IntentSnapshot { epoch, core_machine_id, active_machines, route_bindings,
  serving_target_entries, authorized_users }` — the `intent.get` reply and the
  gateway/DNS fold input, plus the authorized-users grant set (see below).
  Reachable Machines subscribe to `intent.changed`, pull the snapshot, and persist
  it to a small local mirror store, distinct from the core's evidence log. This is
  what makes promotion instant.

- **Authorized-users is store-held operator intent, not disk-as-truth.** The grant
  set (core principals, every machine, and external `User` grants like Cloud) lives
  in the core store as durable operator intent; `authorized-users.conf` is a
  rendered projection of it, reloaded into `nats-server` on change. So the grants
  ride the drumbeat inside `IntentSnapshot` and seed a promoted core's store like
  the roster does — no separate transport, no re-derivation from partial truth. This
  replaces the earlier disk-file-as-source-of-truth writer, whose read-parse-merge
  was the only home of the full grant set and the reason promotion had to rebuild it.

- **A promoted core seeds intent from its mirror and starts evidence fresh — the
  core database is never copied.** This is why the single `core-store.db` mixing
  adoptable intent and mortal evidence needs *no* partition: promotion does not
  adopt the dead core's file, it reconstructs intent from the mirror and opens a
  new, empty evidence log (evidence is mortal with the core, ADR 0029). The
  storage "boundary" is the mirror, not a second database.

- **The machine's NATS client takes a candidate list, via native `async-nats`
  multi-server.** `NatsConnectConfig.url` becomes a list of addresses; `connect`
  passes them all and the client's own reconnect logic cycles them — no custom
  failover loop. The list is populated from the Reachable Machines in the mirrored
  roster. **This seam is blocked on the mirror:** until intent is mirrored, the
  list has exactly one entry, so it lands *after* the mirror, not before.

- **Reachability is observed, then written onto the machine intent record.** The
  core learns a machine's public reachability from its inbound control
  connection's source address (NATS `$SYS` connection events), and records it as a
  field on that machine's intent — so it mirrors with everything else and drives
  the candidate set. No install-time stability flag (ADR 0030).

- **Promotion is `ployz core-promote`: local, idempotent, operator-triggered.** It
  seeds the intent store — roster, routes, serving targets, and the authorized-users
  grant set — from the local mirror, bumps and persists a higher epoch, **reuses the
  succeeded core's controller/operator/join principals from their pre-positioned
  seeds (it does not rotate them)**, self-issues its server TLS cert, and starts
  serving as core. Reuse is what keeps the operator and Cloud authorized after
  recovery: a promoted core is a faithful resurrection of the one it succeeds, so
  every existing credential still works. Nothing auto-elects (ADR 0019/0030); the
  operator runs it over an SSH forced command or by hand.
- **The one cluster secret a candidate needs is the CA signing key, and it rides
  encrypted.** A promoted core at a new address must present a server cert the
  fleet's `tls://` clients trust — signed by the cluster CA, whose key the dead
  disposable core cannot hand over at promote time. So the CA key is pre-positioned
  on Reachable Machines **wrapped with an operator recovery secret** — a passphrase
  KDF-stretched (Argon2id, pinned + version-tagged per blob so a future cost change
  never strands an already-wrapped key). At `init` the secret is either supplied
  via the `PLOYZ_RECOVERY_SECRET` environment variable (e.g. Cloud passing its
  stored value — an env var keeps it out of argv/process listings and shell
  history) or generated by keeper and shown once for the operator to keep; it
  wraps the CA key before it is persisted or mirrored and is itself never stored. A machine compromise yields
  only ciphertext; `core-promote` takes the secret, decrypts the local copy, and
  self-issues.
  The core's own **controller/operator/join principal seeds** ride the same lane —
  wrapped with the recovery secret and pre-positioned in join material — so a
  promoted core reuses the old core's principals rather than minting fresh ones.
  Grants (public keys) are non-secret and travel in the mirror. The recovery secret
  is the one thing the operator must keep — losing it means no promotion. This keeps
  promotion instant (nothing is fetched) while a stolen machine yields only
  ciphertext and cannot impersonate the cluster.

Build order, because each earns the next:

1. **Epoch** — independent; the fence every later seam relies on.
2. **Intent mirror** — machine-side persistence of `IntentSnapshot` off the drumbeat.
3. **Reachability** — the observed fact that populates the candidate set.
4. **Candidate-list connect** — needs 2 + 3 to carry more than one address.
5. **`core-promote`** — needs all of the above.

This rejects:

- **JetStream KV / stream sequence / Raft term as the epoch.** Exited in ADR 0029;
  adopting one re-imports the consensus machinery ADR 0030 rejects. The native
  option is the wrong option here precisely because of our own NATS decisions.
- **Splitting `core-store.db` into separate intent and evidence databases to
  "enable promotion."** Promotion rebuilds intent from the mirror and starts fresh
  evidence, so the file is never copied and never needs cutting along that line.
- **Shipping the candidate list before the mirror.** It would be a length-one list
  that reads as done but recovers nothing — worse than absent, because it looks
  finished.
- **Mirroring the CA key in plaintext** (root on any Reachable Machine would then
  own the cluster's signing key), and equally **BYOK-only** (the operator must have
  the key in hand at promote, forfeiting instant recovery and Cloud's one-button).
  The encrypted-at-rest recovery key is the balance between the two.

Consequences, stated plainly:

- A cluster still needs **at least two Reachable Machines** to have any failover
  target (restating ADR 0030's topology truth).
- The epoch is monotonic only as far as the core persists it **and** the operator
  does not promote two candidates at once. That single-promoter assumption is
  ADR 0030's "deliberate operator act," not a new fence — there is no distributed
  agreement backstopping it.
- The mirror is eventually-consistent off the drumbeat, so a machine promoted with
  a slightly stale mirror serves slightly stale intent until the next operator
  action converges it. Acceptable: facts re-gather live at the point of use
  (ADR 0027), and intent is operator truth that the next mutation refreshes.
- Every Reachable Machine now holds a durable copy of cluster intent — a new small
  on-disk artifact per machine, and the authorized-user set rides the same
  reachability scoping (ADR 0030), so mirror scope is a sensitivity boundary, not
  just an optimization.
- **Reusing the core seeds means a leaked old-core seed stays valid on the promoted
  core** — rotation would have invalidated it. Accepted: the recovery secret already
  wraps the CA key, which can forge *any* identity, so pre-positioning the principal
  seeds under the same secret grants an attacker who holds it nothing new. Rotation
  as a side effect of a rare recovery event was also the wrong tool; deliberate
  credential rotation is a separate, explicit operation. The reuse is what makes
  recovery non-destructive to the operator's and Cloud's existing credentials.
