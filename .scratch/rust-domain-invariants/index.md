# Rust domain invariant task graph

Branch: `feat/rust-domain-invariants`
Base: `e0258e859ed0d71230e41ecf05681fe0b39d7ff4`
PR: https://github.com/getployz/ployz2/pull/739

[Spec](spec.md) · [Machine-readable graph](tickets.json)

| Ticket | Outcome | Blocked by | Status |
|---|---|---|---|
| [T00](tickets/T00-decide-effective-mount-destination-admission.md) | Decide effective mount destination admission | — | complete |
| [T01](tickets/T01-derive-machine-identity-and-validate-local-lifecycle-payloads.md) | Derive Machine identity and validate local lifecycle payloads | — | in_progress |
| [T02](tickets/T02-serialize-reset-and-container-admission.md) | Serialize reset and container admission | T01 | pending |
| [T03](tickets/T03-separate-requested-and-scoped-volume-sources.md) | Separate requested and scoped volume sources | — | in_progress |
| [T04](tickets/T04-check-resource-quantities-and-cpu-conversion.md) | Check resource quantities and CPU conversion | — | in_progress |
| [T05](tickets/T05-require-nonempty-pre-deploy-hook-command.md) | Require nonempty pre-deploy hook command | T04 | pending |
| [T06](tickets/T06-admit-combined-effective-mounts.md) | Admit combined effective mounts | T00, T03, T05 | pending |
| [T07](tickets/T07-make-executable-plans-private-and-derive-progress.md) | Make executable plans private and derive progress | T06 | pending |
| [T08](tickets/T08-reject-known-host-socket-conflicts-during-placement.md) | Reject known host socket conflicts during placement | T07 | pending |
| [T09](tickets/T09-make-container-observations-coherent.md) | Make Container observations coherent | — | in_progress |
| [T10](tickets/T10-validate-row-and-document-identity-at-persistence-admission.md) | Validate row and document identity at persistence admission | T01, T09 | pending |
| [T11](tickets/T11-retain-resource-identity-in-volume-removal-outcomes.md) | Retain resource identity in volume removal outcomes | — | pending |
| [T12](tickets/T12-derive-runtime-watch-services-from-containers.md) | Derive Runtime Watch Services from Containers | T09 | pending |
| [T13](tickets/T13-validate-relay-endpoints-before-pairing-persistence.md) | Validate Relay endpoints before pairing persistence | — | in_progress |
| [T14](tickets/T14-own-and-bound-pending-relay-tunnel-lifetime.md) | Own and bound pending Relay tunnel lifetime | T13 | pending |
| [T15](tickets/T15-validate-certificate-material-in-its-private-constructor.md) | Validate certificate material in its private constructor | — | pending |
| [T16](tickets/T16-validate-http-01-challenge-grammar-and-safe-rendering.md) | Validate HTTP-01 challenge grammar and safe rendering | T15 | pending |
| [T17](tickets/T17-validate-hosted-dns-reservations.md) | Validate hosted DNS reservations | T10 | pending |
| [T18](tickets/T18-encode-ambiguity-minimum-cardinality.md) | Encode ambiguity minimum cardinality | — | in_progress |
| [T19](tickets/T19-integrate-contracts,-evidence,-review-and-pr-readiness.md) | Integrate contracts, evidence, review and PR readiness | T01, T02, T03, T04, T05, T06, T07, T08, T09, T10, T11, T12, T13, T14, T15, T16, T17, T18 | pending |
