# ADR 0035: Fresh Dataplane Testimony Gates New Placement

## Status

Accepted.

## Context

ADR 0027 makes liveness a point-of-use concern and says a placement bid is the
live answer for placement. A bid proves that a machine agent answered, but it
does not prove that the machine can reach the other machines available to the
same placement attempt. New placement must not choose a machine whose endpoint
bridge, WireGuard interface, eBPF attachment, peer set, or peer handshakes are
unusable for that placement attempt.

## Decision

Placement starts from the declared machine set in intent and reads the current
declared Dataplane Projection. At the point of use it requests fresh
machine-scoped facts and dataplane testimony directly from every declared
candidate. It first forms a preliminary placement set from lifecycle-active
machines that answered both requests, named the exact declared revision, and
reported their endpoint bridge, WireGuard interface, and eBPF attachment ready.

Each preliminary candidate may take new workload placement only when its
testimony:

- names the exact declared projection revision;
- reports its endpoint bridge, WireGuard interface, and eBPF attachment ready;
- contains every other preliminary candidate with the expected endpoint
  subnet; and
- reports a WireGuard handshake with every other preliminary candidate no more
  than 275 seconds old.

Declared machines outside the preliminary set remain in durable intent and in
the configured projection, but they are not expected peers for this placement
attempt. Their silence or local failure therefore cannot poison candidates
that are mutually connected. Peer validation runs once against the fixed
preliminary set; exclusions do not trigger recursive shrinking or revalidation.

RTT testimony is not required. Silence, a wrong revision, unusable local
components, a peer-set mismatch, and a missing or stale handshake become typed
Machine Usability Reasons for that placement attempt.

The gather is a fresh request at placement time. Its result is evidence, not
stored liveness or cluster truth. It does not evict a machine, move or stop an
existing workload, or change serving state. Those changes still require their
own explicit operations.

This supersedes ADR 0027 only where it says that a placement bid alone is the
complete live answer for placement. ADR 0027's gateway, DNS, observation-age,
and no-inferred-liveness decisions remain unchanged.

## Consequences

New placement excludes preliminary candidates that are not fully connected to
every other preliminary candidate. Existing workloads keep running from last
committed intent; operators receive the exact point-of-use failure instead of a
durable unhealthy flag. The 275-second handshake bound is protocol policy and
must be changed explicitly if operational evidence later requires another
value.
