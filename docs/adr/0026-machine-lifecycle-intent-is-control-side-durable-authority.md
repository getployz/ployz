# Machine Lifecycle Intent Is Control-Side Durable Authority

Machine lifecycle (active/draining) is operator intent about a machine, not a
machine-owned fact: the machines most worth draining are the ones that may be
unreachable, so the target machine's disk cannot be the commit point. The
drain/resume operation commits intent through the core sequencer before
recording terminal operation evidence. Only non-default intent is recorded;
an absent entry means active. Readers load it through the core-owned intent
service and never depend on its storage representation.

Intent about possibly-dead machines remains control-side regardless of the
store shape used by machine-owned fact ledgers.

Lifecycle affects placement only. A draining machine keeps serving its
running workloads until the next deploy converges placement: its replicas do
not count as existing capacity, so the plan places replacements elsewhere
and cleanup removes the originals. It stays reachable for cleanup; serving
liveness surfaces at the point of use per ADR 0027.
