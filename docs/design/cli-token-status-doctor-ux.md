# Operator UX over the row model: token, status, doctor

Design spec from the wayfinder ticket [Decide: join-token and status/doctor UX
over the row model](https://github.com/getployz/ployz/issues/793). Sits over
[the row model](corrosion-row-model.md) (#785, as amended by #792) and
[the mesh-provider + Principal spec](mesh-provider-and-principal.md) (#787).

**Two facts shape every command here:**

1. **The CLI is a remote mesh peer, not a cluster node.** It connects over
   WireGuard (a roaming `peers` row; userspace WG per the userspace-WG
   research) and reaches one cluster machine's HTTP/JSON endpoint. `status`
   and `doctor` are therefore *remote queries answered by whichever machine
   the CLI reaches* — they report the cluster's health, never "the local
   node's." There is no local node.
2. **IDs are machinery.** Operators select by human handle (name, hostname).
   A raw ULID surfaces only in rare disambiguation, and even then the tool
   prints a **copy-paste-ready** command with the id already substituted — the
   operator copies a whole line, never hand-authors an identifier.

Cloud is **not** a consumer of this CLI surface: it drives the HTTP/JSON API
through the TypeScript SDK (its own not-yet-specified ticket). The CLI is a
human operator's tool. Read commands stay human-table-first; a `--output json`
mode is deferred until a concrete operator-scripting consumer appears (`doctor`
exit code already signals "anomalies found" for CI).

## `ployz token`

One kind of token exists in v1: a join token (the broader signed-writes /
API-token tier is deferred entirely, per #782). The joiner declares machine vs
roaming-peer at the door; the token carries no kind.

```
ployz token create [--ttl 24h]     mint a show-once join credential
ployz token list [--all]           list live tokens (--all includes expired)
ployz token revoke <id>            delete the row (= invalidation + cleanup)
```

### `token create` — show-once

The row keeps only `sha256(secret)`, so creation is the one and only time the
secret can be shown. Output is a single **opaque join blob** —
`pzjoin_<base64 of secret + cluster door-cert fingerprint + member endpoints>`
— printed once, wrapped by paste-ready commands:

```
$ ployz token create --ttl 24h
┌─ save this now — it is shown once ─────────────────────────┐
  pzjoin_eyJ0IjoicHpf...Vd
└────────────────────────────────────────────────────────────┘

  join a machine:   ployz machine join pzjoin_eyJ0IjoicHpf...Vd
  cloud-init:       curl -sSL ployz.sh | sh -s -- join pzjoin_eyJ0IjoicHpf...Vd

  token  01J9..ABC   expires 2026-08-01 14:00 UTC (24h)
```

The blob is the join primitive; the commands are convenience wrappers, and
cloud-init templating interpolates the single blob as one variable. One thing
to copy — you cannot half-copy the token without its cert fingerprint the way
kubeadm's split token/hash allows.

### `token list`

```
$ ployz token list
ID            EXPIRES        CREATED
01J9..ABC     in 22h         2h ago

$ ployz token list --all      # includes expired
01J9..DEF     expired 5h ago  1d ago
```

No use count: a redemption write-back would be a machine writing an
operator-authority row on the join hot path, which the row-ownership law
forbids. Redemptions leave evidence in the `machines`/`peers` rows they create,
never on the token. Expired rows are hidden by default (`--all` reveals them);
they linger until `revoke` or refound.

### `token revoke` = delete

Verification is an O(1) lookup by the ULID embedded in the presented string, so
deleting the row *is* revocation — the lookup fails, the token is dead. Revoke
therefore also serves as expired-token cleanup. There is no separate
`token rm`. **This amends the row model's sweep column** for `tokens`:
`token rm` → `token revoke`.

## `ployz status` — the everyday glance

Cheap, always-safe, run constantly. Answered by the single machine the CLI
reaches from its replicated Corrosion view plus that machine's own live
handshake ages. No cluster-wide sweep (that is `doctor`'s job).

```
$ ployz status
cluster  acme-prod   01J9..CLU        3 machines
sync     caught up   lag p99 480ms                    (as seen from core-1)
barrier  ready

NAME       ROLE            HANDSHAKE    ADDR
core-1     gateway,dns     12s ago      fd00:..:1
edge-a     worker          48s ago      fd00:..:2
edge-b     worker          6m ago  ⚠    fd00:..:3
```

### The three summary lines

- **`sync`** — a verdict (`caught up` / `syncing` / `degraded`) plus the raw
  Corrosion `p99_lag` read from the answering machine's `/v1/health`. The raw
  number is kept literally as the ticket asked; revisit if it proves
  inaccurate/misleading on quiet clusters (the design deleted the *lag-derived
  claim wait* as unproven — see #792 — but displaying the number is only a
  diagnostic, not a coordination input).
- **`barrier`** — tracks **durable roster knowledge, not connectivity**. The
  Corrosion DB is durable on each machine's disk, so a reboot never loses the
  roster.
  - `ready` — non-empty durable roster; the machine will fold and admit. This
    is the normal state **and** the whole-cluster-reboot state.
  - `catching up` — a *brand-new* machine still replicating the initial roster
    snapshot for the first time; reads may be partial, fold/admit held. Names
    the repair: wait, or `ployz doctor`.
  - `no roster` — an **empty/wiped** DB, genuinely nothing to act on (the real
    WG-lockout guard: never fold from an empty roster). Names the repair:
    `ployz machine join <token>` — join is the only door, wiped or fresh; there
    is no rejoin primitive and no identity resurrection (#798). The laptop side
    of the same journey is `machine rm` of the corpse roster row *before* the
    rejoin (the corpse holds the lower ULID and would win the name collision);
    `doctor` names it when it sees the dark machine.
- **connectivity is not a barrier state.** It lives in the `HANDSHAKE` column:
  raw WG last-handshake age, `⚠` past **275s** (the same threshold placement
  uses, so `status` and the scheduler never disagree about who is alive),
  `never` as its own state. When *every* peer is stale, `status` adds a derived
  hint (this machine may be partitioned) naming `ployz network status`.

### Whole-cluster reboot / all-off-then-boot

Because the barrier tracks durable roster knowledge, a simultaneous cold boot
needs zero intervention:

- Each machine reads its persisted Corrosion DB → roster intact → `barrier
  ready` immediately (not waiting to reach anyone) → Keeper folds the persisted
  roster and brings WG up with all peers.
- `HANDSHAKE` shows every peer `never`/stale for the first seconds, clearing to
  live within one handshake interval as peers finish booting. Stale-clearing is
  healthy, not alarming.
- The **remote CLI simply cannot connect** until the first machine is back and
  reachable over WG; then that machine answers with `ready` + clearing
  handshakes. "Cannot reach cluster" (the operator's own WG reaches no machine)
  is a top-level `status`/`doctor` state — the remote analog of lockout, about
  the *operator's* reachability — and names the repair (check
  `ployz network status` / re-join the laptop).

## `ployz doctor` — the deeper sweep

Read-only diagnosis, run when `status` looks wrong or before a risky change. It
pays for the cluster-wide sweeps `status` avoids, and for each anomaly prints
the exact **copy-paste-ready** command that repairs it — running nothing
itself. This is the product's "a refusal never performs work; it names the
command that does" law: `doctor` is the seam that hands off to the repair
primitives. No `doctor --fix` (that would be the forbidden primitive-fusing
command).

`status` hands off to `doctor` by name when it sees something it will not fully
diagnose (all-stale handshakes, `sync degraded`).

### Finding 1 — shadowed rows (lowest-ULID name collision)

A claim loser that never deleted itself (a partition outlasted the courtesy
beat), so two rows carry the same name. Readers already serve the lowest-ULID
winner and ignore the higher-ULID shadow, so this is cleanup, not an outage.

```
$ ployz doctor
⚠ name conflict: namespace "foo" has 2 rows
    01J9..AAA  kept   (lowest ULID — this is the live one)
    01J9..ZZZ  shadowed, inert
  remove the shadow:  ployz namespace rm --id 01J9..ZZZ
```

The everyday `rm` selector stays by-name; each table's `rm` gains an
`--id <ulid>` selector as the collision disambiguator, and `doctor` names the
**higher-ULID** (shadowed) row with the id pre-filled. Removal stays inside the
table's own primitive (which carries that table's sweep rules) — no generic
`row rm` engine.

### Finding 2 — skipped newer-`v` rows (mixed binary versions)

A reader on an older binary sees a row whose `v` exceeds what it understands,
skips it (never guesses), and reports it — the rollout-ordering law violated in
practice. Since the CLI is remote and not in the roster, this is reported
**cluster-wide**: `doctor` sweeps every machine's `machine_status` version,
finds the newest, and names the lagging machines by name.

```
$ ployz doctor
⚠ mixed binary versions in the cluster
    newest:  v0.1.0-alpha.7   (edge-a, core-1)
    behind:  v0.1.0-alpha.5   (edge-b — skipping v=2 rows in `services`)
  roll the lagging machines forward:  ployz machine upgrade edge-b
```

Repair is "upgrade the lagging machine" (caller-paced `{version, sha256, url}`,
Keeper self-update). The command surface is
[the upgrade command spec](upgrade-command-surface.md) (#797): `doctor` names
each lagging machine explicitly, and multiple laggards list as arguments
(`ployz machine upgrade edge-b edge-c`).

### Finding 3 — foreign-`cluster_id` rows (data contamination)

Corrosion gossip rides inside WireGuard and cryptokey routing only admits
roster peers, and join/init refuse foreign on-host state at the door (the four
arrival states, #800/#798), so this finding is a diagnosed disaster, not a
workflow — a member whose `/var/lib/ployz` was restored from another cluster's
backup, disk-cloned, or hand-copied underneath a live daemon. Readers already
drop the rows by `cluster_id` (the data fence), so they are inert. Two
branches:

```
$ ployz doctor
⚠ foreign-cluster rows present (ignored by all readers)
    12 rows carry cluster_id 01J8..OTHER (this cluster is 01J9..CLU)
    authored by: edge-b   ← a machine in your mesh is writing another cluster's id
  edge-b is misconfigured for this cluster; repair it:
    ployz machine rm edge-b                       (fence first — its writes stop)
    on edge-b:  sudo ployz machine reset
    ployz token create   →  paste the join line on edge-b
```

- **Still being authored by a current mesh machine** → the repair-kit
  composition (#798), in order: `machine rm` (the removal fence stops the
  garbage at the source), on-host `machine reset`, rejoin through the ordinary
  token door with a fresh identity. A targeted row delete would not help — it
  re-writes them. There is no SSH driver form and no re-init command; the
  repair is an on-host line, which is also the only shape Cloud can relay
  (Cloud has no SSH path — it renders its own card from the same anomaly and
  composes one paste line, `sudo ployz machine reset && sudo ployz machine
  join pzjoin_…`; paste lines may fuse, commands may not).
- **Static / orphaned** → a note, not an actionable finding: the rows are
  inert and linger harmlessly until a refound (no reaper in v1; nothing to
  run).
