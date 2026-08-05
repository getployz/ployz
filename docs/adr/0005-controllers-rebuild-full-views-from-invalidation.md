# Controllers Rebuild Full Views From Invalidation

Ployz role processes should treat Corrosion subscriptions as invalidation signals, not as authoritative deltas. When gateway, DNS, cert, or machine-facing code sees a relevant change, it should re-query the current scoped rows and rebuild its local projection from that view.

Delta payloads may be used as an optimization, but not as the correctness path. If a view becomes too large to reload cheaply, add scoped records or snapshots before making delta application correctness-critical.
