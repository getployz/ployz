Tailcat is pinned to `91dc4979bd4ae88af6ae2c8bb549616de4bcaa5a`.
`build.sh` fetches that commit and applies `lifecycle.patch` before building.
The patch is local; it has not been accepted upstream. Keep it until an upstream
release provides equivalent bounded admission and idle peer cleanup.

The adapter sets `Server.MaxClients` and `Server.ClientIdleTimeout`. Tailcat
rejects new peers at the cap. Admissions that never authenticate and completed
TCP clients expire after one idle interval plus at most one sweep interval.
Repeated meows do not renew an idle admission. A running TCP handler pins its
peer, including silent streams; the adapter must keep the handler running until
its connection closes. Native TCP keepalive and user timeout use the same idle
interval to terminate vanished clients, including unacknowledged payloads; live
quiet clients answer probes and remain connected. Peer expiry follows handler
completion. Cleanup removes the Tailcat map, magicsock map, netstack
addresses and WireGuard peer through `Engine.SyncDevicePeer`. No endpoint restart
or helper peer registry is used. The optional cleanup mode rejects UDP handlers.
Concurrent meow handlers are bounded to 64; excess admissions retry naturally.

Rung 1: `./build.sh --test` runs `TestBoundedClientLifecycle` against a local DERP
fixture. It fills and expires unauthenticated admissions, checks the cap, attempts
three real wrong-PSK handshakes, churns successful real TCP clients, and exchanges
data on an established stream after each cleanup. The test checks retained
library and engine peer state. Churn uses an injected observation time; final
cleanup exercises the production sweep timer. An abruptly closed client stack
must release its open TCP handler and peer while a quiet survivor remains usable. Run `go test -race -run TestBoundedClientLifecycle .`
in the prepared upstream directory for race instrumentation.

`cargo test -p ployz --test tailcat_connect` runs these lifecycle checks before
the private-DERP contract with ten helper processes and fifty concurrent RPC
streams, cancellation, process termination, and final zero-peer cleanup.

Management uses relay-only networking by default on both the client and server.
Before creating goroutines, `main` calls the pinned Tailscale `envknob.Setenv`
for `TS_DEBUG_ALWAYS_USE_DERP=true`; this also updates the registered magicsock
knob, disabling its UDP sockets. An operator does not need to set an environment
variable. `TestRelayOnlyStartup` exercises production startup in a subprocess
with an externally false value and verifies the safe default wins.

Actual Railway Node22/Linuxx64 qualification found that the same helper could
complete small Inspect replies over its default route to a real Machine while
larger DescribeContract replies stalled (25 seconds in the raw probe, five
seconds at SDK confirmation). Relay-only completed the same SDK read in76ms,
then passed RuntimeWatch, independent sessions, cancellation and deadline cleanup.
`TS_DEBUG_MTU=1200` did not fix the default route; no MTU root cause is claimed.
Direct UDP remains an optional optimization until that path passes qualification.
The adapter uses the upstream routing switch, not a second transport or retry
policy. Preserve this evidence when upgrading Tailcat/Tailscale.
