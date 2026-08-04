# Real-Host Acceptance

The current coreless v2 workspace has no real-host acceptance harness. The
public install, WireGuard/tcx, firewall, and public DNS/TLS paths are not yet
implemented at a stable public seam, so a host script cannot make a truthful
release claim about them.

The product slice that introduces each real-host-only seam owns restoring a
bounded acceptance path in the same change. A restored harness must:

- provision only explicit short-lived test hosts;
- use the shipped public command or HTTP surface;
- bound every external wait and retain failure evidence;
- record the exact candidate version and host inventory;
- state precisely which behaviors and architectures it certifies;
- leave destructive storage or reboot work behind explicit opt-in guards.

Until such a harness exists, record real-host validation as not applicable for
changes proven by deterministic tests and as a blocker for any release claim
that depends on an unimplemented host seam.
