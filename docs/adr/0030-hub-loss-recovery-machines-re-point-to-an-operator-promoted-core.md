# Hub-Loss Recovery: Machines Re-Point Themselves To An Operator-Promoted Core

At the v1 target size every machine is a plain client of one `nats-server` on
the Control-Plane Core, so losing the core loses the bus for everyone — a hard
Hetzner-console delete of the core machine, with no graceful drain, is the
worst case. Recovery must restore the control plane without consensus, without
talking to every node at once (RPC is down), and without making Cloud the
authority that recovers the cluster. This ADR decides how the fleet finds a
new core.

Recovery is **N independent, idempotent, epoch-gated reconnections**, not a
coordinated cutover:

- **Reachability is a declared endpoint the transport corrects, not a
  core-observed source address.** A machine auto-detects and advertises its own
  reachable public endpoint as machine testimony (the public-IP fact it already
  broadcasts) — no operator stability flag, and no source-address observation
  infrastructure, which the flat NATS authorization model cannot cheaply give
  (no `system_account` for `$SYS`; `/connz` is an HTTP side-channel). The core
  records it, and this one fact selects promotion candidates, WireGuard
  dial-targets, and later public-role suitability (e.g. DNS answers); the install
  stays trivially simple. The trust model carries the declaration: a machine that
  misdeclares its address only makes itself a promotion candidate that cannot be
  reached — a failed promotion, surfaced, never silent corruption. When anti-spoof
  matters the correction is added NATS-natively via auth callout — the server
  handing an authorization service each connection's real source IP at connect
  time, the NATS analog of WireGuard learning a peer's real endpoint from its
  handshakes — never a trusted-forever flag.
- **Every machine holds a candidate list and re-points itself.** A machine's
  NATS client is configured with the Reachable Machines from its cached roster,
  not a single hub URL. On core loss it cycles them; the promoted core comes up
  advertising a higher Control-Plane Epoch; the machine connects, adopts it,
  and persists it. Nothing is pushed to N machines — each pulls its way back,
  which is exactly why the bus being down does not block recovery. The Epoch
  fences a healed old core: it sees a higher Epoch and demotes itself.
- **Promotion is a deliberate operator act, and instant.** Per ADR 0019 the
  operator promotes one chosen Reachable Machine; nothing auto-elects, so two
  candidates are a healthy choice, never a race. Promotion is instant rather
  than a backup restore because Reachable Machines mirror the core's intent
  files off the drumbeat.
- **Cloud's button is an optional SSH-triggered local promotion.** Cloud is
  the main consumer, and the one-button flow — "core deleted → select a
  replacement → done" — is Cloud running the local promote command over SSH on
  the chosen Reachable Machine. Granting this is **opt-in**: an operator who
  wants the button lets the install inject Cloud's promotion public key as a
  forced command (`command="ployz core-promote" …`), so the key can only
  trigger promotion, never open a shell, and Cloud's path to the button does
  not route through the dead cluster hub. An operator who does not want a cloud
  service holding SSH access simply declines it and promotes by running the
  same command on the machine directly — copy-paste, no Cloud involved. Either
  way the machine self-authorizes with local root, so Cloud only ever triggers
  and never authorizes. This keeps the Cloud Lens invariant — Cloud is the easy
  button when you opt in, never the required authority — and keeps
  self-hosted clusters free of any cloud-held credential.

This rejects: automatic promotion or leader election (the coordination problem
the disposable-core model exists to avoid); a manual stability classification
(the operator would not read the manual; the machine auto-advertises its
endpoint instead); a standing machine→Cloud heartbeat channel (SSH-at-bootstrap
is simpler given the callback-only install and needs no new outbound protocol);
and Cloud as recovery authority (it holds only a promotion-scoped key).

Consequences, stated plainly:

- A cluster needs **at least two Reachable Machines to have any failover
  target** — with only the core reachable, its loss leaves nowhere to promote.
  This is a topology truth, surfaced to the operator as a redundancy warning
  (the warning itself is a deferred diagnostic slice, not part of this ADR).
- Intent mirroring starts broad — most machines mirror for now — and tightens
  toward Reachable-Machines-only when it matters; the authorized-user set is
  the sensitive part and rides the same reachability scoping.
- During a core outage Cloud shows machines from last-known state (static for
  the duration) and reaches them on demand over SSH, rather than a live lens.
- The reachability fact is deliberately shaped to serve future read consumers —
  recommendation queries and Cloud change hooks — but those are their own
  decisions when Cloud needs them, out of scope here.
