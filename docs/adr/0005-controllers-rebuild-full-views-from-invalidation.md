# Controllers Rebuild Full Views From Invalidation

Ployz controllers and data-plane roles should treat NATS watch events as invalidation signals, not as authoritative deltas. When gateway, DNS, cert, or machine-facing code sees a relevant change, it should reload the current scoped view and rebuild its local projection from that view.

This adopts Uncloud's useful subscription pattern: changes wake consumers up, then consumers re-list state and rebuild. The trade-off is extra reads and re-rendering work in exchange for fewer missed-event, ordering, replay, and per-consumer idempotency problems.

Delta payloads may be used as an optimization, but not as the correctness path. If a view becomes too large to reload cheaply, add scoped records or snapshots before making delta application correctness-critical.
