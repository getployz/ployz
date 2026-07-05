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

- **Reachability is an observed fact, never a declared setting.** A machine is
  a Reachable Machine when it accepts inbound control connections at a public
  address, observed from connection source addresses — the install asks for no
  stability flag. This one fact selects promotion candidates, WireGuard
  dial-targets, and later public-role suitability (e.g. DNS answers); the
  install stays trivially simple.
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
- **Cloud's button is an SSH-triggered local promotion.** Cloud is the main
  consumer, and the one-button flow — "core deleted → select a replacement →
  done" — is Cloud running the local promote command over SSH on the chosen
  Reachable Machine. The install injects Cloud's promotion public key as a
  forced command (`command="ployz core-promote" …`), so the key can only
  trigger promotion, never open a shell. Cloud's connectivity to the button
  therefore does not route through the dead cluster hub, satisfying "Cloud
  must not lose the ability to recover," while the machine self-authorizes
  with local root — so Cloud triggers, and never authorizes. Manual local
  promotion remains the floor: if Cloud is down the operator SSHes in and runs
  the same command. This keeps the Cloud Lens invariant — Cloud is the easy
  button, never the required authority.

This rejects: automatic promotion or leader election (the coordination problem
the disposable-core model exists to avoid); a declared stability classification
(the operator would not read the manual, and reachability is observable); a
standing machine→Cloud heartbeat channel (SSH-at-bootstrap is simpler given the
callback-only install and needs no new outbound protocol); and Cloud as
recovery authority (it holds only a promotion-scoped key).

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
