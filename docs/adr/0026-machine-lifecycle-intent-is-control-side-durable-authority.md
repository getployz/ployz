# Machine Lifecycle Intent Is Control-Side Durable Authority

Machine lifecycle (active/draining) is operator intent about a machine, not a
machine-owned fact: the machines most worth draining are the ones that may be
unreachable, so the target machine's disk cannot be the commit point. The
drain/resume operation commits intent to a control-side evidence file
(`machine-lifecycles.json`, atomic write, one writer) before recording the KV
machine record, and control adopts the file back into KV on start — the same
recovery pattern as the authorized-user set (ADR 0001). Only non-default
intent is recorded; an absent entry means active. The KV record is the
rebuildable projection, never the only home of the intent.

The evidence stays a JSON file rather than SQLite: one single-writer record
set needs no transactions or migrations, and a plainly readable file is
better debugging evidence. When the ADR-0018 machine fact ledger lands, the
control side may adopt the same store shape — but intent about
possibly-dead machines remains control-side regardless.

Lifecycle affects placement only. A draining machine keeps serving its
running workloads until the next deploy converges placement: its replicas do
not count as existing capacity, so the plan places replacements elsewhere
and cleanup removes the originals. It stays reachable for cleanup; serving
liveness surfaces at the point of use per ADR 0027.
