# Corrosion three-node spike results

## Proposed verdict: pass with caveats

Stock Corrosion v1.0.0 formed a three-node cluster on fresh 1 GiB Ubuntu hosts,
replicated the Ployz-shaped workload without a missed row, survived process and
host restarts, and completed both schema-skew recovery and a full-cluster v1
reseed. The fresh certification run completed in under ten minutes.

The result is good enough to continue the coreless design work, but it does not
justify extra machinery. Start with the stock pinned binary as an independent
systemd unit. Do not adopt a custom fork or daemon-supervised container until a
real requirement makes one necessary.

## Certification

- Run: `20260731T070624Z-3724339`
- Topology: three same-region Vultr `vc2-1c-1gb` Ubuntu 24.04 hosts
- Corrosion asset: GitHub `v1.0.0`, archive SHA-256
  `3504d7d1b4b53737457fc40f2353a400cf4df0c1217ec318924d7ee310876194`
- Mesh: WireGuard, Corrosion gossip plaintext inside the mesh, Corrosion
  `max_mtu = 1232`, API bound to loopback
- Evidence: 207 readable JSON records, zero unreadable records
- Cleanup: all certification and development hosts and temporary SSH keys were
  deleted; the Vultr account had no remaining `ployz-wf779-*` resources

## Measurements

| Measure | Result |
| --- | ---: |
| Idle propagation | n=30, p50 517 ms, p99 656 ms |
| Deploy-shaped propagation | n=80, p50 389 ms, p99 622 ms |
| Initial resident memory | 31.5–31.8 MiB per agent |
| Highest observed resident memory | 46.1 MiB |
| Observed lifetime CPU during load | 1.2–3.0% of one vCPU |
| Day-1 DB + WAL + SHM | 5.61 MiB |
| Day-7 DB + WAL + SHM | 7.54 MiB |

Latency is end-to-end from the writer timestamp through the loopback API,
WireGuard replication, and observer query. Chrony residuals were recorded and
applied. The deploy run included 200 background deploy bursts while twenty
measured bursts produced one intent, two route, and one fact observation each.
No latency threshold was invented for this spike.

The simulated week was seven batches of 100 deploy bursts, not seven wall-clock
days. The WAL plateaued near 4.13 MiB while the main DB grew from 1.49 MiB after
day 1 to 3.38 MiB after day 7. A churn drill created seven never-reused fact
IDs; the reaper removed all seven clock and PK records after retention elapsed.
SQLite file sizes did not shrink, so retention controls metadata growth but is
not file compaction.

## Failure drills

- Membership formed on the first barrier check with two peers visible from
  every node.
- A killed Corrosion process restarted automatically; the PID changed and
  systemd's restart count moved from 0 to 1.
- A full host reboot restored SSH, WireGuard, Corrosion, and API access without
  operator repair.
- Subscription reconnect delivered the next five changes. A network partition
  followed by recovery delivered all ten expected rows; every signal was folded
  by a full query.
- 520 rapid writes did not exhaust Corrosion's replay window. They were
  coalesced into the next replay change, and a full query contained the final
  row. The actual typed window-loss response therefore remains unobserved.
- With a new physical column loaded on only two nodes, the third node rejected
  and stalled the new-column batch. Installing and reloading the schema alone
  did not retry it. One Corrosion restart applied the exact stalled row, after
  which all three version vectors matched with zero gaps.
- The pinned binary was replaced atomically one node at a time, with readiness
  checked before continuing.
- A v1 snapshot reseed preserved the pre-snapshot row, omitted the
  post-snapshot row, converged on the first zero-gap barrier check, and accepted
  replay of the omitted row. A root-run restore leaves SQLite sidecars owned by
  root, so the runbook must restore ownership before starting the unprivileged
  service.
- Direct `sqlite3` inspection on two nodes returned identical final kind
  counts, which was enough to diagnose row presence without a special cluster
  debugger.

## Operational rules

Keep the production shape small:

1. Pin one stock v1 asset and checksum with the Ployz release. Upgrade one node,
   verify readiness and convergence, then continue. Do not claim mixed-version
   safety until two adjacent stable v1 releases exist and pass this drill.
2. Use the three JSON-document tables with generated index columns. Unknown
   JSON fields are tolerated; physical schema changes are exceptional.
3. Roll a physical schema to every node and pass a schema barrier before any
   writer uses it. Recovery from a missed node is install, reload, one restart,
   then a zero-gap barrier.
4. Treat subscription events only as invalidations. Re-query the full
   projection on every event, and restart plus resnapshot on typed replay loss.
5. For reseed, freeze or record writes, back up, bump `cluster_id`, restore all
   nodes, restore file ownership, write an idempotency marker, wait for the
   zero-gap barrier, then replay later writes.
6. Never render WireGuard peers from an empty machine roster.

Corrosion's `/v1/health` returned 503 early because it had no p99 lag sample
yet, even though queries and membership worked. Readiness should therefore be
a trivial query plus membership/convergence barriers, not `/v1/health` alone.

## Caveats

- GitHub `v1.0.0` is the only published stable v1 release and its binary reports
  the embedded version `0.2.0-beta.0`. This spike certifies same-version
  replacement and v1 reseed, not an adjacent-version rolling upgrade.
- Replay-window loss could not be forced with 520 rapid writes because Corrosion
  coalesced them. The simple full-query fold is validated; the typed loss branch
  still needs a deterministic upstream-level test before relying on its exact
  error shape.
- Results cover one same-region, three-host x86_64 topology and synthetic
  Ployz-shaped documents. They do not cover cross-region behavior, arm64, or a
  future Corrosion release.
