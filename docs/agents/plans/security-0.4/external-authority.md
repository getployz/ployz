# Existing DNS and GitHub authority

This is the initial source evidence and scope for investigation threads X and Y. Neither thread is an implementation commitment. Findings use commit `38666a984131d1c2a94d624abe5bb1ba8125f474`. No live credential inspection or service mutation was performed. Private `getployz/ops` configuration is unknown.

## X. DNS authority

Hosted DNS already sits behind an HTTP API. Root on np1 can call that API using the same credentials as Cloud. Physical separation alone therefore does not establish containment.

| Credential or API | Source evidence | What np1 can do |
| --- | --- | --- |
| Optional mint key | `dashboard/src/server/config.server.ts:99`, `dashboard/src/modules/cluster-domain/cluster-domain.server.ts:39` | Reserve names within the service's unverified limits. |
| Encrypted per-domain bearer token | `dashboard/src/modules/cluster-domain/tables.ts:50`, `dashboard/src/modules/cluster-domain/sync.server.ts:72` | Decrypt every stored token with Cloud's key and bypass Cloud-side organization checks. |
| Address replacement | `dashboard/src/modules/cluster-domain/hosted-dns.server.ts:74` | Replace a domain's A and AAAA address set, redirecting generated-domain traffic. |
| Retirement and renewal | `dashboard/src/modules/cluster-domain/hosted-dns.server.ts:69` and `:85` | Retire a name or withhold lease renewal. The client describes retirement as permanent. |
| Certificate request | `dashboard/src/modules/cluster-domain/hosted-dns.server.ts:92` | Submit an attacker-key CSR for the name and wildcard, within the actual service's enforced scope. |
| TLS private keys | `dashboard/src/modules/cluster-domain/tables.ts:63`, `dashboard/src/modules/cluster-domain/sync.server.ts:175` and `:219` | Recover existing keys created and stored by Cloud. Redirected generated-domain TLS can be terminated by the attacker. |
| Cluster certificate delivery | `dashboard/src/modules/cluster-domain/sync.server.ts:55` | Publish certificate material through Machine RPC. Server authorization must cover this independent of any external redesign. |

Cloud's normal ingress selection at `sync.server.ts:82` and `:141` is not a root-attacker boundary.

| Customer domain setup | Demonstrated consequence |
| --- | --- |
| Generated hostname | DNS redirection plus generated-domain TLS key access. |
| Custom CNAME pointing at generated hostname | Redirection follows the generated record. The generated wildcard does not itself authenticate the custom hostname. Fresh custom-domain certificate issuance is conditional on DNS and CA behavior. |
| Custom direct A record | The customer manages that record. Generated-record changes do not change it. The attacker still has today's workload-control paths until the server track closes them. |

The custom DNS setup is documented in `docs/user/services/domains.md:100`. No customer custom-domain provider credentials were established by this source review.

### Bounded investigation and decision

X inventories every reserve, sync, renew, certificate, and release caller. It inspects the actual Hosted DNS service implementation and deployed grants if available through authorized read-only access. Missing access becomes an explicit unknown, not inferred enforcement.

X returns a credential-to-endpoint matrix, generated/custom-domain matrix, token and key rotation requirements, and a recommendation among keeping the current boundary, narrowing its API, or separating authority. A proposed external service must reject an arbitrary address, unauthorized retirement, and arbitrary CSR key despite np1 holding every Cloud credential.

A candidate is Machine-held TLS keys and independently authorized domain-to-Cluster binding. It needs proof of initial ownership, allowed address changes, routine renewal, recovery, and service failure behavior. It is not yet selected. A service that accepts a Cloud “approved” flag, organization ID, or unrestricted signing request does not close the attack.

Customer workflow and approval frequency do not change during this investigation. Any later implementation must describe one-time setup and routine renewal without repeated taps. Residual authority must be stated operation by operation.

## Y. GitHub authority

| Credential or API | Source evidence | What np1 can do |
| --- | --- | --- |
| App key and installation-token mint | `dashboard/src/modules/github/github-token.server.ts:17`, `:44`, `:52` | Mint tokens without the repository or permission narrowing omitted by the current request. Reach depends on actual installation grants. |
| Repository reads | `dashboard/src/modules/github/github.api.ts:70` and `:124`, `dashboard/src/modules/github/github-observation.api.ts:306` and `:644` | Read permitted metadata, refs, files, and archives. Existing repository reads remain allowed in this plan. |
| Actions dispatch and cancellation | `dashboard/src/modules/github/github-build.server.ts:63` and `:85` | Trigger or cancel workflows within GitHub's granted permissions. Cloud's normal workflow choice is not an attacker boundary. |
| Checks | `dashboard/src/modules/github/github-observation.api.ts:500`, `dashboard/src/modules/config-store/store-pull-request.server.ts:162` | Create or alter Ployz check output within the App's grant. |
| Webhook secret | `dashboard/src/modules/github/github.api.ts:139`, `dashboard/src/routes/api/github/-webhook.handler.ts:53` | Fabricate deliveries to Cloud. Webhook validation cannot independently authorize a Machine. |
| Connected-account tokens | `dashboard/src/modules/identity/tables.ts:80` | Read token fields. Actual permissions and validity need inventory. |

The documented App grant is Contents read, Actions write, Checks write, Pull requests read, Metadata read, and organization Members read. See `docs/user/self-hosting.md:79`. Production permissions have not been inspected. This does not establish repository or workflow write access. The Add workflow flow opens GitHub's file editor for the user to commit at `dashboard/src/modules/github/github-build-workflow.ts:52`.

The current build dispatch uses Cloud-selected default-branch and build/cloud/runner inputs at `github-build.server.ts:74` and `github-build-workflow.ts:8`. OIDC validation runs inside Cloud with a Cloud-origin audience at `dashboard/src/modules/config-store/store-github-builds.server.ts:229`. That result is insufficient for the new Machine boundary.

### Bounded investigation and decision

Y inventories App, OAuth, and webhook credentials, their actual versus documented permissions, token export, installation reach, every current caller, and the proof used by deploys and previews. The external `getployz/build` action's checkout and output provenance also need inspection where accessible. Its implementation was not established by this checkout review.

Y returns current read/write reach and a recommendation among retaining, narrowing, or separating authority. A proposed broker must not export unrestricted installation tokens or expose an unrestricted GitHub proxy. It must independently constrain whichever mutations it claims to protect. Reads still callable by Cloud remain readable by np1. No new repository-read approval restrictions belong in this work.

Y provides D with evidence about private-repository head verification, workflow identity, reruns, force pushes, and source-to-artifact binding. It does not invent a future coding-agent approval service. If separation is recommended, specify the authority source for current dispatch, cancellation, and check operations and show concrete rejected requests.

## Dependencies, acceptance, and stopping point

X and Y can run in parallel with A. They own separate reports and no runtime files. Their findings gate any external implementation proposal. Y also informs D's independent GitHub proof choice. A and B do not need to wait for a service relocation decision.

The reports must distinguish source-confirmed behavior, actual deployed grants, fixture behavior, and unknowns. Neither investigation requires customer-domain mutations, workflow dispatch, builds, or production secret values in logs. Report key locations and scope, not secrets.

Later DNS implementation evidence needs an isolated real domain and service exercise for record, CSR, and retirement rejection. `verify-dashboard` fakes Hosted DNS and cannot prove public DNS or CA enforcement. `verify-server` proves certificate delivery and Machine admission.

Later GitHub implementation evidence needs an isolated real installation/repository and Actions proof. `verify-dashboard` fakes repository SHAs and issues its own OIDC tokens. Those fixtures prove callers and UI, not GitHub's actual grants or claims. Focused module checks and GitHub CI remain required for any implementation.

The relevant bets are Evidence over claims, Cloud drives, never owns, Machine-local authority, Client-first, daemon when forced, Lean on proven primitives, and Bounded imperative commands. A separate service must establish its own enforcement argument. It cannot borrow trust from np1's database.

Both investigations end at a cited recommendation with remaining blast radius and unresolved operational facts. An implementation thread starts only after its bounded recommendation is selected.
