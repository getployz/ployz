# Swarm slices

Each slice is one verifier thread's session on one fresh Boat Cluster (or one verify-dashboard instance with its own Servers). A slice lists its setup, the skill that drives it, and its entries by id; read each entry in its area file for Reach and Proof. Standalone slices (S) need no Cloud. Cloud slices (C) use a local Cloud, never prod Cloud.

A verifier reports each entry as PASS, ISSUES or BLOCKED with evidence, against the exact SHA it ran. An entry it couldn't drive is BLOCKED or not run, never PASS. A slice with spare time takes the next entry's Compat row onto the 0.3.19 ↔ 0.4 matrix rather than another slice's entries.

| Slice | Covers | Entries |
| --- | --- | --- |
| [S1](#s1) | CLI plumbing and output | 26 |
| [S2](#s2) | Service lifecycle and settings from the CLI | 23 |
| [S3](#s3) | Service runtime: limits, health, restarts, regions, replicas | 21 |
| [S4](#s4) | Deploy pipeline from the CLI | 21 |
| [S5](#s5) | Builds on Servers | 13 |
| [S6](#s6) | Ingress, certificates and private networking | 28 |
| [S7](#s7) | Volumes on one Cluster: storage, Trash, writers, fence | 18 |
| [S8](#s8) | Server lifecycle without Cloud: keys, devices, drain, remove, forget, mesh | 24 |
| [S9](#s9) | Install paths on bare hosts | 20 |
| [C1](#c1) | Sign-in, account, Organizations, members and tokens | 28 |
| [C2](#c2) | Approvals, billing, deleting an Organization, sandboxes, uptime | 14 |
| [C3](#c3) | Agents: `ployz mcp`, remote MCP, the Agent sidebar | 26 |
| [C4](#c4) | Canvas, services, variables, configs and Volumes in the dashboard | 20 |
| [C5](#c5) | Projects, Environments, Branches and Sync | 22 |
| [C6](#c6) | GitHub, Git builds, deploy on push and PR Environments | 30 |
| [C7](#c7) | Deploys, approvals and history through Cloud | 28 |
| [C8](#c8) | Enrollment and joins through Cloud | 27 |
| [C9](#c9) | Server management, upgrades and removal through Cloud | 25 |
| [C10](#c10) | Volume runs: resize, Move, mirror, restore, leftovers | 22 |
| [C11](#c11) | Domains, web traffic and Cluster naming through Cloud | 16 |
| [C12](#c12) | Logs, metrics, analytics and the Collector | 26 |
| [C13](#c13) | Self-hosted Cloud packaging | 5 |

## S1

**CLI plumbing and output.** Setup: `verify-cluster up --daemon checkout --machines 2` (Standalone). Drive: verify-server.

- [CLI-01](cli-and-agents.md#cli-01-see-where-commands-act-ployz-ctx) See where commands act (`ployz ctx`)
- [CLI-02](cli-and-agents.md#cli-02-manage-local-contexts-ployz-ctx-ls-ctx-use-ctx-rm) Manage local contexts (`ployz ctx ls`, `ctx use`, `ctx rm`)
- [CLI-03](cli-and-agents.md#cli-03-pick-a-context-per-command---context--c-ployz_context) Pick a context per command (`--context`, `-c`, `PLOYZ_CONTEXT`)
- [CLI-04](cli-and-agents.md#cli-04-link-a-directory-to-a-project-and-environment) Link a directory to a Project and Environment
- [CLI-05](cli-and-agents.md#cli-05-ployz-status-who-you-are-where-what-is-staged) `ployz status`: who you are, where, what is staged
- [CLI-06](cli-and-agents.md#cli-06-ployz-status-names-servers-that-dont-answer) `ployz status` names servers that don't answer
- [CLI-07](cli-and-agents.md#cli-07-ployz-status-names-replicas-that-arent-serving) `ployz status` names replicas that aren't serving
- [CLI-08](cli-and-agents.md#cli-08-ployz-status-on-a-server-waiting-to-join-and-its-wireguard-key) `ployz status` on a server waiting to join, and its WireGuard key
- [CLI-09](cli-and-agents.md#cli-09-search-the-docs-offline-ployz-docs) Search the docs offline (`ployz docs`)
- [CLI-10](cli-and-agents.md#cli-10---help-names-the-docs-page-and-the-permission) `--help` names the docs page and the permission
- [CLI-11](cli-and-agents.md#cli-11-shell-completion) Shell completion
- [CLI-12](cli-and-agents.md#cli-12-commands-published-by-ployz-cloud) Commands published by Ployz Cloud
- [CLI-13](cli-and-agents.md#cli-13-output-contract-stdout-stderr-piped-lists---json) Output contract: stdout, stderr, piped lists, `--json`
- [CLI-14](cli-and-agents.md#cli-14-retry-lines-and-placeholders) `retry:` lines and placeholders
- [CLI-15](cli-and-agents.md#cli-15-output-never-shows-url-credentials-or-a-mistyped-secret) Output never shows URL credentials or a mistyped secret
- [CLI-16](cli-and-agents.md#cli-16-colors-follow-the-terminal) Colors follow the terminal
- [CLI-17](cli-and-agents.md#cli-17-a-typed-name-confirms-a-deletion) A typed name confirms a deletion
- [CLI-18](cli-and-agents.md#cli-18-a-change-whose-reply-was-lost-says-it-may-have-applied) A change whose reply was lost says it may have applied
- [AGT-07](cli-and-agents.md#agt-07-a-coding-agents-cli-run-needs---why) A coding agent's CLI run needs `--why`
- [SVC-03](services.md#svc-03-list-services-and-what-the-next-deploy-does-to-them) List services and what the next deploy does to them
- [SVC-04](services.md#svc-04-inspect-one-service) Inspect one service
- [SVC-05](services.md#svc-05-rename-a-service-and-keep-its-private-name) Rename a service and keep its private name
- [SVC-06](services.md#svc-06-ployz-refuses-the-reserved-service-names-configs-volumes-and-setup) Ployz refuses the reserved service names configs, volumes and setup
- [SVC-16](services.md#svc-16-read-a-services-settings-from-the-cli) Read a service's settings from the CLI
- [SVC-17](services.md#svc-17-stage-and-unset-settings-from-the-cli) Stage and unset settings from the CLI
- [SVC-18](services.md#svc-18-explain-a-setting-and-print-the-settings-schema) Explain a setting and print the settings schema

## S2

**Service lifecycle and settings from the CLI.** Setup: `verify-cluster up --daemon checkout --machines 2` (Standalone). Drive: verify-server.

- [SVC-07](services.md#svc-07-delete-a-service-and-see-what-it-breaks) Delete a service and see what it breaks
- [SVC-10](services.md#svc-10-restart-a-service-one-container-at-a-time) Restart a service one container at a time
- [SVC-11](services.md#svc-11-stop-a-service-until-you-start-or-deploy-it) Stop a service until you start or deploy it
- [SVC-12](services.md#svc-12-start-a-stopped-service) Start a stopped service
- [SVC-13](services.md#svc-13-run-a-command-or-a-shell-in-a-service) Run a command or a shell in a service
- [SVC-14](services.md#svc-14-list-an-environments-containers-across-servers) List an environment's containers across servers
- [SVC-15](services.md#svc-15-forward-a-local-port-to-a-service) Forward a local port to a service
- [SVC-23](services.md#svc-23-change-a-services-private-address) Change a service's private address
- [SVC-27](services.md#svc-27-override-the-start-command) Override the start command
- [SVC-28](services.md#svc-28-run-a-pre-deploy-command-before-each-deploy) Run a pre-deploy command before each deploy
- [SVC-32](services.md#svc-32-set-a-stop-signal-and-grace-period) Set a stop signal and grace period
- [SVC-35](services.md#svc-35-rename-a-variable-and-its-references) Rename a variable and its references
- [SVC-36](services.md#svc-36-seal-a-secret-from-the-cli) Seal a secret from the CLI
- [SVC-40](services.md#svc-40-generate-a-random-value) Generate a random value
- [SVC-41](services.md#svc-41-reference-another-services-variable) Reference another service's variable
- [SVC-42](services.md#svc-42-a-broken-reference-is-flagged-and-left-out-of-deploys) A broken reference is flagged and left out of deploys
- [SVC-43](services.md#svc-43-variables-ployz-provides) Variables Ployz provides
- [SVC-44](services.md#svc-44-ployz-refuses-variables-linux-cant-start) Ployz refuses variables Linux can't start
- [SVC-46](services.md#svc-46-manage-configs-from-the-cli) Manage configs from the CLI
- [SVC-47](services.md#svc-47-write-a-config-file-with-its-permissions-from-the-cli) Write a config file with its permissions from the CLI
- [SVC-51](services.md#svc-51-ployz-refuses-configs-too-large-to-mount) Ployz refuses configs too large to mount
- [SVC-52](services.md#svc-52-tell-apart-a-deleted-config-and-its-same-named-replacement) Tell apart a deleted config and its same-named replacement
- [SVC-55](services.md#svc-55-back-up-and-restore-a-database-from-the-cli) Back up and restore a database from the CLI

## S3

**Service runtime: limits, health, restarts, regions, replicas.** Setup: `verify-cluster up --daemon checkout --machines 3` (Standalone); label regions with `server set --label-add region=`. Drive: verify-server; verify-region-placement, verify-service-floor.

- [SVC-24](services.md#svc-24-run-more-replicas-one-per-server-first) Run more replicas, one per server first
- [SVC-25](services.md#svc-25-servers-apply-cpu-and-memory-limits-to-each-replica) Servers apply CPU and memory limits to each replica
- [SVC-26](services.md#svc-26-a-limit-change-applies-in-place-without-a-restart) A limit change applies in place without a restart
- [SVC-29](services.md#svc-29-a-healthcheck-gates-a-new-replicas-traffic) A healthcheck gates a new replica's traffic
- [SVC-30](services.md#svc-30-servers-restart-a-crashed-replica-by-its-restart-policy) Servers restart a crashed replica by its restart policy
- [SVC-31](services.md#svc-31-choose-how-a-deploy-replaces-replicas) Choose how a deploy replaces replicas
- [SVC-56](services.md#svc-56-a-deploy-leaves-waiting-a-service-whose-volumes-server-takes-no-services) A deploy leaves waiting a service whose volume's server takes no services
- [SVC-57](services.md#svc-57-bring-services-back-when-a-server-goes-down) Bring services back when a server goes down
- [SVC-58](services.md#svc-58-run-a-service-in-more-than-one-region) Run a service in more than one region
- [SVC-59](services.md#svc-59-a-service-whose-volume-is-outside-the-region-waits) A service whose volume is outside the region waits
- [SVC-60](services.md#svc-60-replicas-wait-while-their-region-has-no-server) Replicas wait while their region has no server
- [SVC-61](services.md#svc-61-services-that-share-a-volume-must-run-in-one-region) Services that share a volume must run in one region
- [SVC-62](services.md#svc-62-global-services-catch-up-on-a-new-server) Global services catch up on a new server
- [SVC-63](services.md#svc-63-a-server-starts-a-container-only-from-a-deploy-a-trusted-key-still-signs) A server starts a container only from a deploy a trusted key still signs
- [SVC-38](services.md#svc-38-sealed-values-open-only-on-the-servers) Sealed values open only on the servers
- [ENV-25](environments.md#env-25-set-an-environments-region) Set an environment's region
- [ENV-26](environments.md#env-26-a-new-environment-runs-in-the-first-servers-region) A new environment runs in the first server's region
- [ENV-09](environments.md#env-09-branch-to-fix-a-failed-deployment) Branch to fix a failed deployment
- [ENV-11](environments.md#env-11-a-setup-command-seeds-a-new-branch) A setup command seeds a new branch
- [ENV-17](environments.md#env-17-secrets-and-generated-values-never-sync) Secrets and generated values never sync
- [ENV-20](environments.md#env-20-productions-deploys-follow-into-a-branch) Production's deploys follow into a branch

## S4

**Deploy pipeline from the CLI.** Setup: `verify-cluster up --daemon checkout --machines 3` (Standalone); `power off/on`, `partition` for DEP-47/48. Drive: verify-server; verify-runner-standing, verify-drain-copy.

- [DEP-02](deploy.md#dep-02-review-staged-changes-from-the-cli) Review staged changes from the CLI
- [DEP-04](deploy.md#dep-04-deploy-everything-staged-from-the-cli) Deploy everything staged from the CLI
- [DEP-05](deploy.md#dep-05-deploy-only-some-services) Deploy only some services
- [DEP-06](deploy.md#dep-06-a-full-deploy-prunes-services-the-environment-doesnt-hold) A full deploy prunes services the environment doesn't hold
- [DEP-08](deploy.md#dep-08-removing-a-service-while-a-server-is-down) Removing a service while a Server is down
- [DEP-10](deploy.md#dep-10-discard-one-staged-change-or-all-of-them) Discard one staged change or all of them
- [DEP-11](deploy.md#dep-11-a-deploy-skips-not-set-secrets-and-broken-references) A deploy skips Not set secrets and broken references
- [DEP-12](deploy.md#dep-12-recreate-every-container-of-a-deploy) Recreate every container of a deploy
- [DEP-13](deploy.md#dep-13-sign-a-deploy-on-an-approved-device) Sign a deploy on an approved device
- [DEP-15](deploy.md#dep-15-a-deploy-pins-a-public-tag-to-its-digest-and-refuses-a-private-tag) A deploy pins a public tag to its digest and refuses a private tag
- [DEP-20](deploy.md#dep-20-refuse-to-sign-when-a-server-cant-check-signed-versions) Refuse to sign when a Server can't check signed versions
- [DEP-21](deploy.md#dep-21-after-a-server-upgrade-old-containers-need-one-signed-deploy) After a Server upgrade, old containers need one signed deploy
- [DEP-30](deploy.md#dep-30-a-bad-image-fails-at-pull-and-keeps-the-old-version) A bad image fails at pull and keeps the old version
- [DEP-35](deploy.md#dep-35-list-and-inspect-deployments-from-the-cli) List and inspect deployments from the CLI
- [DEP-36](deploy.md#dep-36-follow-a-deploy-in-the-cli-with-exit-codes) Follow a deploy in the CLI with exit codes
- [DEP-42](deploy.md#dep-42-undo-or-restore-a-saved-version-from-the-cli) Undo or restore a saved version from the CLI
- [DEP-44](deploy.md#dep-44-start-first-replace-keeps-the-app-serving) Start-first replace keeps the app serving
- [DEP-46](deploy.md#dep-46-pre-deploy-command-and-dependency-order) Pre-deploy command and dependency order
- [DEP-47](deploy.md#dep-47-a-deploy-when-a-server-is-cut-off) A deploy when a Server is cut off
- [DEP-48](deploy.md#dep-48-a-deploy-survives-ployzd-restarting-mid-deploy-and-mid-retirement) A deploy survives ployzd restarting mid-deploy and mid-retirement
- [DEP-49](deploy.md#dep-49-deploy-a-directory-with-ployz-up) Deploy a directory with ployz up

## S5

**Builds on Servers.** Setup: `verify-cluster up --daemon checkout --machines 2` (Standalone). BLD-07 needs an ARM Machine the lab lacks: report it not run. Drive: verify-server.

- [BLD-01](builds.md#bld-01-build-an-app-with-railpack-no-dockerfile) Build an app with Railpack, no Dockerfile
- [BLD-04](builds.md#bld-04-variables-reach-the-build) Variables reach the build
- [BLD-06](builds.md#bld-06-a-build-cant-reach-the-private-network-or-metadata) A build can't reach the private network or metadata
- [BLD-07](builds.md#bld-07-servers-that-mix-x86-and-arm) Servers that mix x86 and ARM
- [BLD-08](builds.md#bld-08-ployz-reuses-an-image-when-nothing-the-build-reads-changed) Ployz reuses an image when nothing the build reads changed
- [BLD-11](builds.md#bld-11-give-builds-more-time-or-time-out-a-waiting-build) Give builds more time, or time out a waiting build
- [BLD-12](builds.md#bld-12-cap-a-builds-cpu-memory-and-cache-with-buildyaml) Cap a build's CPU, memory and cache with build.yaml
- [BLD-13](builds.md#bld-13-clear-the-build-cache) Clear the build cache
- [BLD-14](builds.md#bld-14-old-built-images-and-unused-free-build-images-are-cleaned-up) Old built images and unused free-build images are cleaned up
- [BLD-15](builds.md#bld-15-a-builder-stays-quarantined-while-an-unconfirmed-build-still-runs) A builder stays quarantined while an unconfirmed build still runs
- [BLD-21](builds.md#bld-21-prefer-a-builder-for-one-service) Prefer a builder for one service
- [BLD-22](builds.md#bld-22-source-size-submodule-and-lfs-refusals) Source size, submodule and LFS refusals
- [BLD-23](builds.md#bld-23-each-uploaded-service-keeps-its-latest-upload) Each uploaded service keeps its latest upload

## S6

**Ingress, certificates and private networking.** Setup: `verify-cluster up --daemon checkout --machines 2` (Standalone). NET-11 needs a real name (sslip.io on a Boat IPv6 host); NET-24/25 a Cloudflare account; NET-41/43 a Tailscale tailnet. Drive: verify-server; verify-acme-issuance, verify-caddy-load, verify-bridge-notes.

- [NET-05](networking.md#net-05-ployz-refuses-a-hostname-that-isnt-a-custom-domain-you-own) Ployz refuses a hostname that isn't a custom domain you own
- [NET-06](networking.md#net-06-see-each-domains-status-and-re-check-it) See each domain's status and re-check it
- [NET-09](networking.md#net-09-ployz-refuses-a-domain-another-environment-still-serves) Ployz refuses a domain another Environment still serves
- [NET-10](networking.md#net-10-a-signed-service-keeps-its-domains) A signed Service keeps its domains
- [NET-11](networking.md#net-11-servers-get-and-renew-https-certificates) Servers get and renew https certificates
- [NET-12](networking.md#net-12-the-ingress-proxy-follows-route-changes-and-survives-caddy-restarts) The Ingress Proxy follows route changes and survives Caddy restarts
- [NET-13](networking.md#net-13-ployz-answers-with-its-own-error-pages-when-it-cant-reach-the-app) Ployz answers with its own error pages when it can't reach the app
- [NET-14](networking.md#net-14-requests-fail-over-when-a-replicas-server-powers-off) Requests fail over when a replica's Server powers off
- [NET-15](networking.md#net-15-trace-a-request-to-see-which-server-answered) Trace a request to see which Server answered
- [NET-16](networking.md#net-16-apps-see-the-real-visitor-address-never-a-forged-one) Apps see the real visitor address, never a forged one
- [NET-20](networking.md#net-20-turn-the-ingress-role-on-and-off-from-the-cli) Turn the ingress role on and off from the CLI
- [NET-21](networking.md#net-21-a-deploy-brings-back-a-missing-ingress-proxy) A deploy brings back a missing Ingress Proxy
- [NET-22](networking.md#net-22-tell-ployz-a-servers-public-ip) Tell Ployz a Server's public IP
- [NET-23](networking.md#net-23-see-which-doors-each-ingress-server-opens) See which Doors each ingress Server opens
- [NET-24](networking.md#net-24-switch-every-ingress-server-to-the-cloudflare-tunnel-from-the-cli) Switch every ingress Server to the Cloudflare Tunnel from the CLI
- [NET-25](networking.md#net-25-go-back-to-open-ports-from-the-cli) Go back to open ports from the CLI
- [NET-26](networking.md#net-26-start-a-new-cluster-with-its-ports-closed) Start a new Cluster with its ports closed
- [NET-31](networking.md#net-31-services-find-each-other-at-nameinternal) Services find each other at NAME.internal
- [NET-32](networking.md#net-32-services-on-different-servers-reach-each-other-over-wireguard) Services on different Servers reach each other over WireGuard
- [NET-33](networking.md#net-33-a-container-keeps-its-private-address-across-docker-and-server-restarts) A container keeps its private address across Docker and Server restarts
- [NET-34](networking.md#net-34-containers-and-the-ingress-proxy-get-ipv6) Containers and the Ingress Proxy get IPv6
- [NET-35](networking.md#net-35-only-signed-containers-reach-a-service-that-holds-a-volume) Only signed containers reach a Service that holds a Volume
- [NET-36](networking.md#net-36-ping-and-raw-sockets-dont-work-inside-a-service) ping and raw sockets don't work inside a Service
- [NET-37](networking.md#net-37-a-private-name-that-ran-signed-stays-signed) A private name that ran signed stays signed
- [NET-39](networking.md#net-39-name-or-rename-a-standalone-cluster) Name or rename a Standalone Cluster
- [NET-41](networking.md#net-41-reach-the-private-network-from-tailscale-with-the-cli) Reach the private network from Tailscale with the CLI
- [NET-43](networking.md#net-43-turn-private-access-off) Turn Private Access off
- [OBS-33](observe.md#obs-33-see-which-server-answered-well-knownployztrace) See which Server answered (`/.well-known/ployz/trace`)

## S7

**Volumes on one Cluster: storage, Trash, writers, fence.** Setup: `verify-cluster up --daemon checkout --machines 3` (Standalone). Drive: verify-server; verify-volume-marks, verify-pool-ownership.

- [VOL-02](volumes.md#vol-02-add-a-volume-from-the-cli) Add a Volume from the CLI
- [VOL-04](volumes.md#vol-04-list-an-environments-volumes-and-what-the-next-deploy-does) List an Environment's Volumes and what the next deploy does
- [VOL-05](volumes.md#vol-05-inspect-one-volume) Inspect one Volume
- [VOL-06](volumes.md#vol-06-rename-a-volume) Rename a Volume
- [VOL-07](volumes.md#vol-07-set-the-storage-limit-before-the-first-deploy) Set the storage limit before the first deploy
- [VOL-10](volumes.md#vol-10-share-a-volume-between-writers) Share a Volume between writers
- [VOL-11](volumes.md#vol-11-one-container-writes-to-a-managed-volume) One container writes to a Managed Volume
- [VOL-16](volumes.md#vol-16-deleted-data-waits-in-the-servers-trash-for-7-days) Deleted data waits in the Server's trash for 7 days
- [VOL-17](volumes.md#vol-17-list-a-servers-trash) List a Server's trash
- [VOL-18](volumes.md#vol-18-get-deleted-data-back-from-the-trash) Get deleted data back from the trash
- [VOL-19](volumes.md#vol-19-empty-a-servers-trash) Empty a Server's trash
- [VOL-22](volumes.md#vol-22-a-deploy-wont-start-a-service-whose-volume-has-no-writer) A deploy won't start a Service whose Volume has no writer
- [VOL-23](volumes.md#vol-23-restore-a-volumes-writer-without-ployz-cloud) Restore a Volume's writer without Ployz Cloud
- [VOL-42](volumes.md#vol-42-a-server-opens-a-volumes-data-only-for-the-service-approved-for-it) A Server opens a Volume's data only for the Service approved for it
- [VOL-43](volumes.md#vol-43-a-new-volume-stays-on-the-server-its-first-deploy-chose) A new Volume stays on the Server its first deploy chose
- [VOL-45](volumes.md#vol-45-list-and-prune-the-anonymous-volumes-a-server-kept) List and prune the anonymous volumes a Server kept
- [SVC-08](services.md#svc-08-deleting-a-databases-service-deletes-its-volume-after-a-confirm) Deleting a database's service deletes its volume, after a confirm
- [SVC-09](services.md#svc-09-anonymous-volumes-go-only-when-a-new-container-takes-the-old-ones-place) Anonymous volumes go only when a new container takes the old one's place

## S8

**Server lifecycle without Cloud: keys, devices, drain, remove, forget, mesh.** Setup: `verify-cluster up --daemon checkout --machines 3` (Standalone); `add` for SRV-72; `power`, `partition` for SRV-58/69/71. Drive: verify-server; verify-member-removal, verify-upgrade-attempt.

- [SRV-02](servers.md#srv-02-add-a-server-over-ssh-from-an-approved-device) Add a server over SSH from an approved device
- [SRV-08](servers.md#srv-08-start-a-standalone-cluster-without-cloud) Start a standalone Cluster without Cloud
- [SRV-21](servers.md#srv-21-see-every-server-and-its-status) See every server and its status
- [SRV-23](servers.md#srv-23-show-one-servers-telemetry-round-trips-and-last-upgrade) Show one server's telemetry, round trips and last upgrade
- [SRV-26](servers.md#srv-26-list-and-remove-devices-and-passkeys-for-good) List and remove devices and passkeys for good
- [SRV-29](servers.md#srv-29-make-a-ci-or-agent-key) Make a CI or agent key
- [SRV-30](servers.md#srv-30-narrow-or-widen-a-keys-scope) Narrow or widen a key's scope
- [SRV-31](servers.md#srv-31-rotate-a-ci-or-agent-key) Rotate a CI or agent key
- [SRV-32](servers.md#srv-32-remove-a-leaked-ci-or-agent-key) Remove a leaked CI or agent key
- [SRV-33](servers.md#srv-33-change-a-servers-name-labels-roles-public-ip-or-build-concurrency) Change a server's name, labels, roles, public IP or build concurrency
- [SRV-36](servers.md#srv-36-turn-web-traffic-on-or-off-from-the-cli) Turn web traffic on or off from the CLI
- [SRV-39](servers.md#srv-39-drain-a-server-from-the-cli) Drain a server from the CLI
- [SRV-42](servers.md#srv-42-set-a-servers-region-from-the-cli) Set a server's region from the CLI
- [SRV-50](servers.md#srv-50-upgrade-or-downgrade-to-an-exact-version-over-ssh) Upgrade or downgrade to an exact version over SSH
- [SRV-55](servers.md#srv-55-remove-a-server-from-the-cli) Remove a server from the CLI
- [SRV-57](servers.md#srv-57-forget-a-dead-server-from-the-cli) Forget a dead server from the CLI
- [SRV-58](servers.md#srv-58-a-forgotten-server-stays-out) A forgotten server stays out
- [SRV-67](servers.md#srv-67-a-server-picks-up-its-new-address-after-a-restart) A server picks up its new address after a restart
- [SRV-68](servers.md#srv-68-tell-servers-where-one-is-after-every-ip-changed) Tell servers where one is after every IP changed
- [SRV-69](servers.md#srv-69-servers-reach-each-other-over-the-wireguard-mesh-and-commands-name-a-missing-handshake) Servers reach each other over the WireGuard mesh, and commands name a missing handshake
- [SRV-71](servers.md#srv-71-ployz-puts-its-firewall-rules-back-within-a-minute) Ployz puts its firewall rules back within a minute
- [SRV-72](servers.md#srv-72-a-joining-server-runs-global-catch-up) A joining server runs Global catch-up
- [SRV-73](servers.md#srv-73-a-server-whose-cluster-store-was-lost-refills-it) A server whose cluster store was lost refills it
- [SRV-75](servers.md#srv-75-a-server-restored-from-a-disk-snapshot-picks-its-services-back-up) A server restored from a disk snapshot picks its services back up

## S9

**Install paths on bare hosts.** Setup: `verify-cluster up --bare --machines 2 --daemon checkout` (and `--distro debian-12`); a published install line needs Phase A assets (soak Release phases). Drive: verify-server (Bare hosts).

- [INS-01](install.md#ins-01-install-or-upgrade-the-cli-with-one-line) Install or upgrade the CLI with one line
- [INS-03](install.md#ins-03-install-a-server-with-a-fleet-token-at-a-hidden-prompt) Install a server with a Fleet Token at a hidden prompt
- [INS-04](install.md#ins-04-the-install-line-carries-the-key-the-server-trusts-first) The install line carries the key the server trusts first
- [INS-05](install.md#ins-05-install-from-cloud-init-user-data) Install from cloud-init user data
- [INS-06](install.md#ins-06-fleet-token-install-outcomes-and-reruns) Fleet Token install outcomes and reruns
- [INS-07](install.md#ins-07-point-an-install-or-the-cli-at-your-own-cloud) Point an install or the CLI at your own Cloud
- [INS-08](install.md#ins-08-install-without-zfs-docker-only) Install without ZFS (Docker only)
- [INS-09](install.md#ins-09-put-managed-volumes-on-an-empty-spare-disk) Put managed volumes on an empty spare disk
- [INS-10](install.md#ins-10-build-zfs-where-the-distribution-doesnt-ship-it) Build ZFS where the distribution doesn't ship it
- [INS-11](install.md#ins-11-refuse-a-host-that-cant-run-ployz) Refuse a host that can't run Ployz
- [INS-12](install.md#ins-12-stop-before-changing-a-docker-ployz-cant-use) Stop before changing a Docker Ployz can't use
- [INS-13](install.md#ins-13-refuse-dockers-nftables-firewall) Refuse Docker's nftables firewall
- [INS-14](install.md#ins-14-a-server-that-runs-firewalld-needs-udp-51820-and-ployz-wg-trusted) A server that runs firewalld needs UDP 51820 and ployz-wg trusted
- [INS-15](install.md#ins-15-block-a-server-while-another-program-holds-udp-51820) Block a server while another program holds UDP 51820
- [INS-16](install.md#ins-16-install-on-an-ipv6-only-server) Install on an IPv6-only server
- [INS-17](install.md#ins-17-install-from-a-test-copy-of-the-release-assets) Install from a test copy of the release assets
- [INS-18](install.md#ins-18-uninstall-ployz-from-a-server) Uninstall Ployz from a server
- [VOL-13](volumes.md#vol-13-put-managed-volumes-on-an-empty-spare-disk) Put Managed volumes on an empty spare disk
- [SRV-51](servers.md#srv-51-refuse-an-unsigned-release-and-use-test-download-mirrors) Refuse an unsigned release and use test download mirrors

## C1

**Sign-in, account, Organizations, members and tokens.** Setup: verify-dashboard `MAIL=1 scripts/verify/up.sh` (no Servers) plus the CLI against it with the seed's tokens. Drive: verify-dashboard; verify-sign-in-store.

- [ACC-01](account.md#acc-01-sign-in-to-ployz-cloud-with-github) Sign in to Ployz Cloud with GitHub
- [ACC-02](account.md#acc-02-sign-the-cli-in-with-ployz-login) Sign the CLI in with `ployz login`
- [ACC-03](account.md#acc-03-finish-a-pending-sign-in-from-a-script---json---wait---cloud-url) Finish a pending sign-in from a script (`--json`, `--wait`, `--cloud-url`)
- [ACC-04](account.md#acc-04-sign-this-computer-out-with-ployz-logout) Sign this computer out with `ployz logout`
- [ACC-05](account.md#acc-05-an-old-or-unreadable-sign-in-is-replaced) An old or unreadable sign-in is replaced
- [ACC-06](account.md#acc-06-keep-the-sign-in-where-you-choose-ployz_config-xdg_config_home) Keep the sign-in where you choose (`PLOYZ_CONFIG`, `XDG_CONFIG_HOME`)
- [ACC-07](account.md#acc-07-see-your-profile-and-sign-in-methods) See your profile and sign-in methods
- [ACC-08](account.md#acc-08-sign-a-browser-out-from-sessions) Sign a browser out from Sessions
- [ACC-09](account.md#acc-09-add-a-passkey-on-the-account-page) Add a passkey on the Account page
- [ACC-10](account.md#acc-10-choose-which-emails-you-get) Choose which emails you get
- [ACC-11](account.md#acc-11-turn-an-email-off-from-the-email-itself) Turn an email off from the email itself
- [ACC-12](account.md#acc-12-a-cloud-without-email-says-so) A Cloud without email says so
- [ACC-13](account.md#acc-13-failed-github-deploy-and-failed-upgrade-emails) Failed GitHub deploy and failed upgrade emails
- [ACC-14](account.md#acc-14-list-and-switch-organizations-from-the-cli) List and switch Organizations from the CLI
- [ACC-15](account.md#acc-15-make-another-organization) Make another Organization
- [ACC-16](account.md#acc-16-rename-the-organization) Rename the Organization
- [ACC-17](account.md#acc-17-invite-someone-with-a-link) Invite someone with a link
- [ACC-18](account.md#acc-18-manage-the-team-from-the-cli-ployz-org-member-ployz-org-invite) Manage the team from the CLI (`ployz org member`, `ployz org invite`)
- [ACC-19](account.md#acc-19-the-last-owner-stays) The last owner stays
- [ACC-20](account.md#acc-20-change-a-role-or-remove-a-member-in-the-dashboard) Change a role or remove a member in the dashboard
- [ACC-21](account.md#acc-21-leave-an-organization) Leave an Organization
- [ACC-22](account.md#acc-22-ployz-says-which-role-can-do-it) Ployz says which role can do it
- [ACC-23](account.md#acc-23-let-ployz-support-in) Let Ployz support in
- [ACC-24](account.md#acc-24-make-an-organization-token-with-a-preset) Make an Organization Token with a preset
- [ACC-25](account.md#acc-25-a-deploy-or-read-token-is-held-to-its-preset) A Deploy or Read token is held to its preset
- [ACC-26](account.md#acc-26-list-and-revoke-tokens-and-signed-in-computers) List and revoke tokens and signed-in computers
- [ACC-27](account.md#acc-27-a-token-cant-make-or-revoke-tokens) A token can't make or revoke tokens
- [ACC-28](account.md#acc-28-ployz_token-tolerates-paste-whitespace) `PLOYZ_TOKEN` tolerates paste whitespace

## C2

**Approvals, billing, deleting an Organization, sandboxes, uptime.** Setup: verify-dashboard `BILLING=1 SERVERS=1 scripts/verify/up.sh`. Drive: verify-dashboard; verify-approval-binding.

- [ACC-29](account.md#acc-29-say-yes-before-something-is-removed) Say yes before something is removed
- [ACC-30](account.md#acc-30-answer-approvals-from-the-cli-ployz-org-approval) Answer approvals from the CLI (`ployz org approval`)
- [ACC-31](account.md#acc-31-upgrade-to-production-and-manage-billing) Upgrade to Production and manage billing
- [ACC-32](account.md#acc-32-without-production-uptime-and-analytics-ask-for-it) Without Production, uptime and analytics ask for it
- [ACC-33](account.md#acc-33-delete-an-organization-from-the-dashboard) Delete an Organization from the dashboard
- [ACC-34](account.md#acc-34-delete-an-organization-with-ployz-org-rm) Delete an Organization with `ployz org rm`
- [ACC-35](account.md#acc-35-get-an-agent-sandbox-without-an-account) Get an Agent sandbox without an account
- [ACC-36](account.md#acc-36-keep-a-sandbox-with-its-claim-link) Keep a sandbox with its claim link
- [ACC-37](account.md#acc-37-some-things-wait-for-a-claim) Some things wait for a claim
- [ACC-38](account.md#acc-38-an-unclaimed-sandbox-ends-after-24-hours) An unclaimed sandbox ends after 24 hours
- [OBS-24](observe.md#obs-24-uptime-from-the-cli-and-agents) Uptime from the CLI and agents
- [OBS-25](observe.md#obs-25-a-4xx-at-the-healthcheck-path-shows-how-to-fix-it) A 4xx at the healthcheck path shows how to fix it
- [OBS-22](observe.md#obs-22-a-services-uptime-on-the-monitoring-tab) A service's uptime on the Monitoring tab
- [OBS-26](observe.md#obs-26-turn-on-web-analytics) Turn on web analytics

## C3

**Agents: `ployz mcp`, remote MCP, the Agent sidebar.** Setup: verify-dashboard `BILLING=1 SERVERS=2 scripts/verify/up.sh`; the sidebar needs `ANTHROPIC_API_KEY`. Drive: verify-dashboard, verify-server; verify-mcp-login, verify-approval-binding.

- [AGT-01](cli-and-agents.md#agt-01-give-a-coding-agent-ployz-tools-with-ployz-mcp) Give a coding agent Ployz tools with `ployz mcp`
- [AGT-02](cli-and-agents.md#agt-02-ployz-mcp-login-mints-its-own-agent-token) `ployz mcp login` mints its own agent token
- [AGT-03](cli-and-agents.md#agt-03-an-agent-token-cant-approve-anything) An agent token can't approve anything
- [AGT-04](cli-and-agents.md#agt-04-connect-an-agent-app-to-the-remote-mcp) Connect an agent app to the remote MCP
- [AGT-05](cli-and-agents.md#agt-05-disconnect-a-connected-agent) Disconnect a connected agent
- [AGT-06](cli-and-agents.md#agt-06-every-tool-call-says-why-intent) Every tool call says why (`intent`)
- [AGT-08](cli-and-agents.md#agt-08-a-resent-write-runs-once-request_key) A resent write runs once (`request_key`)
- [AGT-09](cli-and-agents.md#agt-09-agents-send-feedback) Agents send feedback
- [AGT-10](cli-and-agents.md#agt-10-an-agents-deploy-waits-on-an-approval-link) An agent's deploy waits on an approval link
- [AGT-11](cli-and-agents.md#agt-11-stops-restarts-and-multi-step-proposals) Stops, restarts and multi-step proposals
- [AGT-12](cli-and-agents.md#agt-12-other-removals-from-an-agent-need-a-person) Other removals from an agent need a person
- [AGT-13](cli-and-agents.md#agt-13-ployz-mcp-stops-a-call-after-30-minutes) `ployz mcp` stops a call after 30 minutes
- [AGT-14](cli-and-agents.md#agt-14-ask-an-agent-for-status-service_status-docs) Ask an agent for status (`service_status`, `docs`)
- [AGT-15](cli-and-agents.md#agt-15-the-agent-tool-list-follows-ployz-cloud) The agent tool list follows Ployz Cloud
- [AGT-16](cli-and-agents.md#agt-16-chat-with-the-agent-sidebar) Chat with the Agent sidebar
- [AGT-17](cli-and-agents.md#agt-17-the-agent-sidebar-acts-where-you-are-looking) The Agent sidebar acts where you are looking
- [AGT-18](cli-and-agents.md#agt-18-sidebar-deploy-and-run-cards) Sidebar deploy and run cards
- [AGT-19](cli-and-agents.md#agt-19-protected-environment-requests-wait-for-an-owner-or-admin) Protected Environment requests wait for an owner or admin
- [AGT-20](cli-and-agents.md#agt-20-agents-never-see-a-secrets-value) Agents never see a secret's value
- [AGT-21](cli-and-agents.md#agt-21-untrusted-content-cant-steer-the-sidebar-agent) Untrusted content can't steer the sidebar agent
- [ENV-35](environments.md#env-35-show-an-environment-to-an-agent) Show an environment to an agent
- [ENV-36](environments.md#env-36-check-what-an-environment-still-needs-before-it-can-deploy) Check what an environment still needs before it can deploy
- [SRV-76](servers.md#srv-76-an-agent-asks-to-drain-remove-or-forget-a-server) An agent asks to drain, remove or forget a server
- [VOL-37](volumes.md#vol-37-the-agent-removes-every-read-only-copy-at-once) The agent removes every read-only copy at once
- [OBS-11](observe.md#obs-11-agents-read-logs-service_logs-server_logs) Agents read logs (`service_logs`, `server_logs`)
- [OBS-18](observe.md#obs-18-agents-read-metrics-service_metrics-server_metrics) Agents read metrics (`service_metrics`, `server_metrics`)

## C4

**Canvas, services, variables, configs and Volumes in the dashboard.** Setup: verify-dashboard `SERVERS=1 scripts/verify/up.sh`. Drive: verify-dashboard.

- [SVC-01](services.md#svc-01-add-a-service-from-an-image-a-repository-or-nothing) Add a service from an image, a repository or nothing
- [SVC-02](services.md#svc-02-find-your-way-around-a-service-on-the-canvas) Find your way around a service on the canvas
- [SVC-19](services.md#svc-19-switch-a-service-between-an-image-and-a-repository) Switch a service between an image and a repository
- [SVC-20](services.md#svc-20-an-empty-service-needs-a-source-before-it-deploys) An empty service needs a source before it deploys
- [SVC-22](services.md#svc-22-pull-a-private-image-with-sealed-registry-credentials) Pull a private image with sealed registry credentials
- [SVC-33](services.md#svc-33-add-change-or-remove-a-variable) Add, change or remove a variable
- [SVC-34](services.md#svc-34-paste-many-variables-at-once) Paste many variables at once
- [SVC-37](services.md#svc-37-seal-a-secret-in-the-dashboard) Seal a secret in the dashboard
- [SVC-39](services.md#svc-39-a-seal-hides-the-earlier-plain-value-in-branches-below) A seal hides the earlier plain value in branches below
- [SVC-45](services.md#svc-45-create-a-config-and-write-its-files) Create a config and write its files
- [SVC-48](services.md#svc-48-mount-a-config-into-services) Mount a config into services
- [SVC-49](services.md#svc-49-reference-variables-inside-a-config-file) Reference variables inside a config file
- [SVC-50](services.md#svc-50-review-config-changes-only-mounting-services-restart) Review config changes; only mounting services restart
- [SVC-53](services.md#svc-53-add-a-database-with-a-sealed-password) Add a database with a sealed password
- [SVC-54](services.md#svc-54-a-database-copied-into-a-branch-needs-a-new-password) A database copied into a branch needs a new password
- [VOL-01](volumes.md#vol-01-add-a-volume-in-the-dashboard-and-mount-it-on-a-service) Add a Volume in the dashboard and mount it on a Service
- [VOL-03](volumes.md#vol-03-ployz-refuses-a-mount-that-breaks-the-mount-rules) Ployz refuses a mount that breaks the mount rules
- [VOL-12](volumes.md#vol-12-use-a-plain-docker-volume-when-no-server-has-managed-volumes) Use a plain Docker volume when no Server has Managed volumes
- [VOL-14](volumes.md#vol-14-detach-a-volume-from-a-service) Detach a Volume from a Service
- [VOL-15](volumes.md#vol-15-delete-a-volume-and-confirm-its-data-goes) Delete a Volume and confirm its data goes

## C5

**Projects, Environments, Branches and Sync.** Setup: verify-dashboard `SERVERS=1 scripts/verify/up.sh`. Drive: verify-dashboard; verify-removal-marks.

- [ENV-01](environments.md#env-01-list-the-organizations-projects) List the organization's projects
- [ENV-02](environments.md#env-02-create-a-project) Create a project
- [ENV-03](environments.md#env-03-rename-a-project) Rename a project
- [ENV-04](environments.md#env-04-delete-a-project) Delete a project
- [ENV-05](environments.md#env-05-change-the-projects-default-environment) Change the project's default environment
- [ENV-06](environments.md#env-06-list-a-projects-environments) List a project's environments
- [ENV-07](environments.md#env-07-start-an-empty-environment) Start an empty environment
- [ENV-08](environments.md#env-08-branch-an-environment-choosing-what-to-copy) Branch an environment, choosing what to copy
- [ENV-10](environments.md#env-10-a-branch-starts-with-its-secrets-not-set) A branch starts with its secrets Not set
- [ENV-12](environments.md#env-12-give-every-new-branch-the-same-setup-commands) Give every new branch the same setup commands
- [ENV-13](environments.md#env-13-a-branch-gets-its-own-generated-address) A branch gets its own generated address
- [ENV-14](environments.md#env-14-make-a-shared-service-separate-in-a-branch) Make a shared service separate in a branch
- [ENV-15](environments.md#env-15-sync-a-branchs-changes-into-its-parent) Sync a branch's changes into its parent
- [ENV-16](environments.md#env-16-sync-into-any-other-environment) Sync into any other environment
- [ENV-18](environments.md#env-18-keep-a-setting-out-of-every-sync) Keep a setting out of every sync
- [ENV-19](environments.md#env-19-take-a-sync-back-out) Take a sync back out
- [ENV-21](environments.md#env-21-keep-a-branch-or-close-it) Keep a branch, or close it
- [ENV-22](environments.md#env-22-protect-an-environment) Protect an environment
- [ENV-23](environments.md#env-23-members-ask-an-owner-or-admin-in-a-protected-environment) Members ask an owner or admin in a Protected environment
- [ENV-24](environments.md#env-24-a-deploy-token-in-a-protected-environment-cant-destroy) A Deploy token in a Protected environment can't destroy
- [ENV-27](environments.md#env-27-delete-an-environment) Delete an environment
- [ENV-28](environments.md#env-28-shut-an-environment-down-and-bring-it-back) Shut an environment down and bring it back

## C6

**GitHub, Git builds, deploy on push and PR Environments.** Setup: verify-dashboard `FAKE_GITHUB=1 SERVERS=2 scripts/verify/up.sh`. Drive: verify-dashboard; verify-build-handoff.

- [BLD-02](builds.md#bld-02-change-the-build-and-start-commands) Change the build and start commands
- [BLD-03](builds.md#bld-03-build-with-your-own-dockerfile) Build with your own Dockerfile
- [BLD-05](builds.md#bld-05-a-build-reads-sealed-values) A build reads sealed values
- [BLD-09](builds.md#bld-09-read-the-build-log) Read the build log
- [BLD-10](builds.md#bld-10-choose-which-servers-run-builds-and-how-many-at-once) Choose which Servers run builds and how many at once
- [BLD-16](builds.md#bld-16-builds-of-services-that-reach-no-data-need-no-approval) Builds of services that reach no data need no approval
- [BLD-17](builds.md#bld-17-add-the-github-actions-build-workflow-to-a-repository) Add the GitHub Actions build workflow to a repository
- [BLD-18](builds.md#bld-18-build-on-github-actions-and-push-to-a-server) Build on GitHub Actions and push to a Server
- [BLD-19](builds.md#bld-19-github-skips-a-build-and-the-next-builder-takes-it) GitHub skips a build and the next builder takes it
- [BLD-20](builds.md#bld-20-choose-the-organizations-build-order) Choose the Organization's build order
- [BLD-24](builds.md#bld-24-connect-a-github-account-to-the-organization) Connect a GitHub account to the Organization
- [BLD-25](builds.md#bld-25-connect-github-from-the-cli-or-an-agent) Connect GitHub from the CLI or an agent
- [BLD-26](builds.md#bld-26-list-and-disconnect-github-accounts-from-the-cli) List and disconnect GitHub accounts from the CLI
- [BLD-27](builds.md#bld-27-read-a-repositorys-files-from-the-cli-or-an-agent) Read a repository's files from the CLI or an agent
- [DEP-50](deploy.md#dep-50-add-a-service-from-a-github-repository) Add a service from a GitHub repository
- [DEP-51](deploy.md#dep-51-choose-the-git-branch-and-protect-who-changes-it) Choose the Git branch and protect who changes it
- [DEP-52](deploy.md#dep-52-deploy-on-push) Deploy on push
- [DEP-53](deploy.md#dep-53-approve-an-automatic-deploy-rule-in-the-dashboard) Approve an automatic deploy rule in the dashboard
- [DEP-54](deploy.md#dep-54-ployz-deploy-signs-the-automatic-deploy-rule) ployz deploy signs the automatic deploy rule
- [DEP-55](deploy.md#dep-55-end-the-automatic-deploy-rule-from-the-cli) End the automatic deploy rule from the CLI
- [DEP-56](deploy.md#dep-56-wait-for-ci-before-a-push-deploys) Wait for CI before a push deploys
- [DEP-57](deploy.md#dep-57-deploy-only-when-watched-paths-change) Deploy only when watched paths change
- [DEP-58](deploy.md#dep-58-deploy-one-app-of-a-monorepo) Deploy one app of a monorepo
- [ENV-29](environments.md#env-29-turn-on-pr-environments-for-a-repository) Turn on PR environments for a repository
- [ENV-30](environments.md#env-30-approve-an-outside-pull-requests-preview) Approve an outside pull request's preview
- [ENV-31](environments.md#env-31-a-previews-build-is-refused-in-this-version) A preview's build is refused in this version
- [ENV-32](environments.md#env-32-what-each-pull-requests-preview-gets) What each pull request's preview gets
- [ENV-33](environments.md#env-33-review-and-edit-what-a-pull-requests-merge-lands) Review and edit what a pull request's merge lands
- [ENV-34](environments.md#env-34-a-merge-lands-the-previews-changes-in-production) A merge lands the preview's changes in production
- [SVC-21](services.md#svc-21-deploy-on-push-settings-take-effect-at-once) Deploy-on-push settings take effect at once

## C7

**Deploys, approvals and history through Cloud.** Setup: verify-dashboard `SERVERS=2 scripts/verify/up.sh`; passkey entries need a WebAuthn authenticator (agent-browser virtual authenticator). Drive: verify-dashboard; verify-approval-binding, verify-runner-standing.

- [DEP-01](deploy.md#dep-01-stage-a-change-and-see-it-counted-in-the-bottom-bar) Stage a change and see it counted in the bottom bar
- [DEP-03](deploy.md#dep-03-review-changes-in-details-and-deploy-them-from-the-dashboard) Review changes in Details and deploy them from the dashboard
- [DEP-07](deploy.md#dep-07-a-deploy-token-cant-prune-and-an-agents-prune-asks-first) A Deploy token can't prune, and an agent's prune asks first
- [DEP-09](deploy.md#dep-09-save-changes-without-deploying) Save changes without deploying
- [DEP-14](deploy.md#dep-14-a-signed-deploy-checks-each-volume-mounts-data) A signed deploy checks each volume mount's data
- [DEP-16](deploy.md#dep-16-each-server-admits-only-signed-versions-of-services-that-reach-data) Each Server admits only signed versions of services that reach data
- [DEP-17](deploy.md#dep-17-ship-a-new-image-without-a-terminal) Ship a new image without a terminal
- [DEP-18](deploy.md#dep-18-deploy-from-ci-with-a-ci-key) Deploy from CI with a CI key
- [DEP-19](deploy.md#dep-19-approve-a-keyless-deploy-from-a-link-with-a-passkey) Approve a keyless deploy from a link with a passkey
- [DEP-22](deploy.md#dep-22-deploy-from-the-dashboard-with-a-passkey) Deploy from the dashboard with a passkey
- [DEP-23](deploy.md#dep-23-forget-older-versions) Forget older versions
- [DEP-24](deploy.md#dep-24-deploy-every-image-pushed-to-one-repository-without-a-prompt) Deploy every image pushed to one repository without a prompt
- [DEP-25](deploy.md#dep-25-add-a-service-from-a-docker-image) Add a service from a Docker image
- [DEP-26](deploy.md#dep-26-ship-a-new-version-of-an-image-from-ci) Ship a new version of an image from CI
- [DEP-27](deploy.md#dep-27-servers-pull-by-digest-and-only-you-write-tags) Servers pull by digest and only you write tags
- [DEP-28](deploy.md#dep-28-use-a-private-image-with-sealed-registry-credentials) Use a private image with sealed registry credentials
- [DEP-29](deploy.md#dep-29-a-server-wont-send-registry-credentials-where-they-could-leak) A Server won't send registry credentials where they could leak
- [DEP-31](deploy.md#dep-31-override-the-start-command) Override the start command
- [DEP-32](deploy.md#dep-32-one-deployment-at-a-time-queued-deploy-next-superseded) One deployment at a time: Queued, Deploy next, Superseded
- [DEP-33](deploy.md#dep-33-follow-a-deployment-on-its-page) Follow a deployment on its page
- [DEP-34](deploy.md#dep-34-cleanup-of-old-replicas-after-a-dashboard-deploy) Cleanup of old replicas after a dashboard deploy
- [DEP-37](deploy.md#dep-37-cancel-a-deployment) Cancel a deployment
- [DEP-38](deploy.md#dep-38-start-a-queued-deployment-now) Start a queued deployment now
- [DEP-39](deploy.md#dep-39-redeploy-a-finished-deployment) Redeploy a finished deployment
- [DEP-40](deploy.md#dep-40-roll-back-to-an-older-deployment) Roll back to an older deployment
- [DEP-41](deploy.md#dep-41-undo-or-roll-back-a-saved-version-in-versions) Undo or roll back a saved version in Versions
- [DEP-43](deploy.md#dep-43-confirm-a-deploy-that-deletes-data) Confirm a deploy that deletes data
- [DEP-45](deploy.md#dep-45-choose-the-update-order) Choose the update order

## C8

**Enrollment and joins through Cloud.** Setup: `verify-cluster up --cloud --bare --machines 3 --daemon checkout`, then each entry's install line or `server add`. Drive: verify-server, verify-dashboard.

- [SRV-01](servers.md#srv-01-add-a-server-by-pasting-one-line) Add a server by pasting one line
- [SRV-03](servers.md#srv-03-put-a-new-server-in-a-region) Put a new server in a region
- [SRV-04](servers.md#srv-04-approve-a-join-from-the-cli-with-its-join-code) Approve a join from the CLI with its join code
- [SRV-05](servers.md#srv-05-approve-a-join-with-a-passkey-in-the-dashboard) Approve a join with a passkey in the dashboard
- [SRV-06](servers.md#srv-06-refuse-to-sign-a-join-relayed-through-an-unpinned-cloud) Refuse to sign a join relayed through an unpinned Cloud
- [SRV-07](servers.md#srv-07-trust-the-key-on-an-install-line-and-replace-it-only-at-the-servers-terminal) Trust the key on an install line, and replace it only at the Server's terminal
- [SRV-09](servers.md#srv-09-approve-a-device-on-the-first-server) Approve a device on the first server
- [SRV-10](servers.md#srv-10-get-an-install-command-for-the-first-server-from-the-dashboard) Get an install command for the first server from the dashboard
- [SRV-11](servers.md#srv-11-name-the-cluster-and-start-it-behind-cloudflare-tunnel) Name the Cluster and start it behind Cloudflare Tunnel
- [SRV-12](servers.md#srv-12-make-list-and-revoke-fleet-tokens) Make, list and revoke Fleet Tokens
- [SRV-13](servers.md#srv-13-add-servers-dialog-with-shell-and-cloud-init-lines) Add servers dialog with Shell and cloud-init lines
- [SRV-14](servers.md#srv-14-review-servers-waiting-to-join) Review servers waiting to join
- [SRV-15](servers.md#srv-15-enroll-waiting-servers-with-the-enroll-sheet) Enroll waiting servers with the Enroll sheet
- [SRV-16](servers.md#srv-16-follow-cancel-and-retry-enrollments) Follow, cancel and retry enrollments
- [SRV-17](servers.md#srv-17-enroll-servers-on-arrival) Enroll servers on arrival
- [SRV-18](servers.md#srv-18-list-and-inspect-waiting-servers-from-the-cli) List and inspect waiting servers from the CLI
- [SRV-19](servers.md#srv-19-enroll-waiting-servers-from-the-cli) Enroll waiting servers from the CLI
- [SRV-20](servers.md#srv-20-reject-waiting-or-enrolling-servers-from-the-cli) Reject waiting or enrolling servers from the CLI
- [SRV-24](servers.md#srv-24-members-are-refused-server-access-through-cloud) Members are refused Server access through Cloud
- [SRV-25](servers.md#srv-25-request-and-approve-a-device) Request and approve a device
- [SRV-27](servers.md#srv-27-pin-your-clusters-key-before-sealing-through-cloud) Pin your Cluster's key before sealing through Cloud
- [SRV-28](servers.md#srv-28-add-a-passkey-and-make-servers-trust-it) Add a passkey and make servers trust it
- [SRV-74](servers.md#srv-74-retry-a-failed-add-and-see-servers-that-never-joined) Retry a failed add, and see servers that never joined
- [SRV-59](servers.md#srv-59-add-a-removed-or-forgotten-server-back-with-a-reset) Add a removed or forgotten server back with a reset
- [SRV-60](servers.md#srv-60-forget-every-server-after-deleting-them-at-the-provider) Forget every server after deleting them at the provider
- [SRV-61](servers.md#srv-61-reset-a-founding-attempt-that-never-finished) Reset a founding attempt that never finished
- [SRV-62](servers.md#srv-62-reset-a-founding-attempt-from-the-cli) Reset a founding attempt from the CLI

## C9

**Server management, upgrades and removal through Cloud.** Setup: verify-dashboard `SERVERS=3 DAEMON="checkout machine-3=0.3.19" scripts/verify/up.sh` so one Server is behind. Drive: verify-dashboard, verify-server; verify-upgrade-attempt, verify-member-removal.

- [SRV-22](servers.md#srv-22-see-what-runs-on-a-server) See what runs on a server
- [SRV-70](servers.md#srv-70-cloud-reaches-servers-through-the-relay) Cloud reaches servers through the relay
- [SRV-34](servers.md#srv-34-stop-new-services-starting-on-a-server) Stop new services starting on a server
- [SRV-35](servers.md#srv-35-turn-web-traffic-off-or-on-from-the-server-page) Turn web traffic off or on from the server page
- [SRV-37](servers.md#srv-37-turn-builds-on-or-off-and-set-builds-at-once) Turn builds on or off and set builds at once
- [SRV-38](servers.md#srv-38-drain-a-server-from-the-dashboard) Drain a server from the dashboard
- [SRV-40](servers.md#srv-40-a-drain-moves-a-services-volume-with-it) A drain moves a service's volume with it
- [SRV-41](servers.md#srv-41-set-a-servers-region-from-its-page) Set a server's region from its page
- [SRV-43](servers.md#srv-43-list-and-rename-regions) List and rename regions
- [SRV-44](servers.md#srv-44-rename-a-region-in-the-dashboard) Rename a region in the dashboard
- [SRV-45](servers.md#srv-45-upgrade-every-server-thats-behind-from-the-servers-page) Upgrade every server that's behind from the Servers page
- [SRV-46](servers.md#srv-46-upgrade-servers-automatically-on-the-release-channel) Upgrade servers automatically on the Release Channel
- [SRV-47](servers.md#srv-47-upgrade-one-server-from-its-page) Upgrade one server from its page
- [SRV-48](servers.md#srv-48-choose-stable-or-beta-releases) Choose Stable or Beta releases
- [SRV-49](servers.md#srv-49-upgrade-servers-from-the-cli-through-cloud) Upgrade servers from the CLI through Cloud
- [SRV-52](servers.md#srv-52-features-name-the-server-that-needs-an-upgrade) Features name the server that needs an upgrade
- [SRV-53](servers.md#srv-53-upgrade-a-server-too-old-for-the-relay-over-ssh-or-on-the-server) Upgrade a server too old for the relay over SSH or on the server
- [SRV-54](servers.md#srv-54-remove-a-server-from-the-dashboard) Remove a server from the dashboard
- [SRV-56](servers.md#srv-56-forget-an-offline-server-from-the-dashboard) Forget an offline server from the dashboard
- [SRV-63](servers.md#srv-63-clean-up-leftovers-of-a-deleted-environment) Clean up leftovers of a deleted environment
- [SRV-64](servers.md#srv-64-get-and-rotate-the-recovery-code) Get and rotate the recovery code
- [SRV-65](servers.md#srv-65-recover-the-sealing-key-on-a-server-that-needs-recovery) Recover the Sealing Key on a server that needs recovery
- [SRV-66](servers.md#srv-66-a-stopped-or-powered-off-server-shows-offline) A stopped or powered-off server shows Offline
- [VOL-44](volumes.md#vol-44-a-drain-moves-each-service-with-its-volume) A drain moves each Service with its Volume
- [VOL-46](volumes.md#vol-46-remove-a-namespace-no-environment-owns) Remove a Namespace no Environment owns

## C10

**Volume runs: resize, Move, mirror, restore, leftovers.** Setup: `verify-cluster up --cloud --machines 3 --daemon checkout` with `probe` on a Postgres Service. Drive: verify-server; verify-volume-runs, verify-drain-copy, verify-volume-marks.

- [VOL-08](volumes.md#vol-08-grow-a-deployed-volume-while-its-service-runs) Grow a deployed Volume while its Service runs
- [VOL-09](volumes.md#vol-09-shrink-a-deployed-volume) Shrink a deployed Volume
- [VOL-20](volumes.md#vol-20-when-a-volumes-server-goes-down-its-services-wait-and-the-mirror-can-take-over) When a Volume's Server goes down, its Services wait and the mirror can take over
- [VOL-21](volumes.md#vol-21-copies-shows-where-a-volumes-data-lives) Copies shows where a Volume's data lives
- [VOL-24](volumes.md#vol-24-restore-a-volumes-writer-through-ployz-cloud) Restore a Volume's writer through Ployz Cloud
- [VOL-25](volumes.md#vol-25-restoring-a-point-into-a-new-volume-is-turned-off) Restoring a point into a new Volume is turned off
- [VOL-26](volumes.md#vol-26-move-a-volume-and-its-service-to-another-server) Move a Volume and its Service to another Server
- [VOL-27](volumes.md#vol-27-a-move-is-refused-before-anything-stops-when-the-destination-cant-start-the-service) A Move is refused before anything stops when the destination can't start the Service
- [VOL-28](volumes.md#vol-28-a-failed-or-stuck-run-is-released) A failed or stuck run is released
- [VOL-29](volumes.md#vol-29-a-volume-takes-one-run-at-a-time-and-a-deploy-takes-a-move-over) A Volume takes one run at a time, and a deploy takes a Move over
- [VOL-30](volumes.md#vol-30-list-and-read-a-volumes-runs) List and read a Volume's runs
- [VOL-31](volumes.md#vol-31-add-a-mirror-in-the-dashboard) Add a mirror in the dashboard
- [VOL-32](volumes.md#vol-32-mirror-a-volume-from-the-cli) Mirror a Volume from the CLI
- [VOL-33](volumes.md#vol-33-refresh-a-mirror-by-hand-or-every-15-minutes) Refresh a mirror, by hand or every 15 minutes
- [VOL-34](volumes.md#vol-34-copy-a-mirror-again-in-full) Copy a mirror again in full
- [VOL-35](volumes.md#vol-35-remove-a-mirror-in-the-dashboard) Remove a mirror in the dashboard
- [VOL-36](volumes.md#vol-36-delete-a-mirror-or-old-copy-from-the-cli) Delete a mirror or old copy from the CLI
- [VOL-38](volumes.md#vol-38-delete-an-old-copy-from-the-dashboard) Delete an old copy from the dashboard
- [VOL-39](volumes.md#vol-39-servers-keep-a-copy-that-may-hold-writes-no-other-server-has) Servers keep a copy that may hold writes no other Server has
- [VOL-40](volumes.md#vol-40-list-the-copies-deleted-volumes-left-behind) List the copies deleted Volumes left behind
- [VOL-41](volumes.md#vol-41-delete-a-leftover-copy-in-two-steps) Delete a leftover copy in two steps
- [VOL-47](volumes.md#vol-47-a-protected-environment-holds-a-members-volume-runs-for-an-owner) A Protected Environment holds a member's Volume runs for an owner

## C11

**Domains, web traffic and Cluster naming through Cloud.** Setup: verify-dashboard `SERVERS=2 scripts/verify/up.sh` (Hosted DNS is fake: `*.ployz.test`, throwaway CA). NET-27..30 and NET-42 need Cloudflare or Tailscale. Drive: verify-dashboard; verify-acme-issuance.

- [NET-01](networking.md#net-01-generate-a-domain-for-a-service-in-the-dashboard) Generate a domain for a Service in the dashboard
- [NET-02](networking.md#net-02-add-another-generated-domain-or-change-ones-prefix-from-the-cli) Add another generated domain, or change one's prefix, from the CLI
- [NET-03](networking.md#net-03-choose-the-port-a-domain-reaches) Choose the port a domain reaches
- [NET-04](networking.md#net-04-add-a-custom-domain-in-the-dashboard-and-see-its-dns-records) Add a custom domain in the dashboard and see its DNS records
- [NET-07](networking.md#net-07-remove-a-domain-it-keeps-serving-until-the-deploy) Remove a domain; it keeps serving until the deploy
- [NET-08](networking.md#net-08-move-a-domain-to-another-service-in-one-deploy) Move a domain to another Service in one deploy
- [NET-17](networking.md#net-17-the-organizations-domain-and-its-port-80-check) The organization's domain and its port 80 check
- [NET-18](networking.md#net-18-hosted-dns-points-generated-domains-at-the-servers-taking-web-traffic) Hosted DNS points generated domains at the Servers taking web traffic
- [NET-19](networking.md#net-19-turn-a-servers-web-traffic-off-and-on-in-the-dashboard) Turn a Server's web traffic off and on in the dashboard
- [NET-27](networking.md#net-27-set-up-cloudflare-and-switch-web-traffic-from-the-dashboard) Set up Cloudflare and switch web traffic from the dashboard
- [NET-28](networking.md#net-28-switch-back-to-public-ports-from-the-dashboard) Switch back to public ports from the dashboard
- [NET-29](networking.md#net-29-choose-a-name-and-cloudflare-tunnel-before-the-first-server) Choose a name and Cloudflare Tunnel before the first Server
- [NET-30](networking.md#net-30-a-custom-domain-behind-the-cloudflare-tunnel) A custom domain behind the Cloudflare Tunnel
- [NET-38](networking.md#net-38-change-a-services-private-name) Change a Service's private name
- [NET-40](networking.md#net-40-rename-the-cluster-through-ployz-cloud) Rename the Cluster through Ployz Cloud
- [NET-42](networking.md#net-42-connect-tailscale-from-the-dashboard) Connect Tailscale from the dashboard

## C12

**Logs, metrics, analytics and the Collector.** Setup: verify-dashboard `SERVERS=1` with ClickHouse running (`--profile observe` or `OBSERVE_CLICKHOUSE_URL`); without it, report stored-data entries not run. Drive: verify-dashboard, verify-server.

- [OBS-01](observe.md#obs-01-watch-an-environments-logs-live) Watch an Environment's logs live
- [OBS-02](observe.md#obs-02-search-stored-logs) Search stored logs
- [OBS-03](observe.md#obs-03-a-services-logs-tab) A service's Logs tab
- [OBS-04](observe.md#obs-04-a-deployments-build-and-container-logs) A deployment's build and container logs
- [OBS-05](observe.md#obs-05-read-logs-from-the-cli-straight-from-the-servers) Read logs from the CLI, straight from the Servers
- [OBS-06](observe.md#obs-06-follow-logs-across-a-redeploy-and-a-lost-server) Follow logs across a redeploy and a lost server
- [OBS-07](observe.md#obs-07-logs-of-a-removed-service-come-from-cloud) Logs of a removed service come from Cloud
- [OBS-08](observe.md#obs-08-logs-of-one-deployment-and-its-build) Logs of one deployment and its build
- [OBS-09](observe.md#obs-09-members-and-tokens-read-logs-through-cloud) Members and tokens read logs through Cloud
- [OBS-10](observe.md#obs-10-read-a-servers-daemon-logs-ployz-server-logs) Read a Server's daemon logs (`ployz server logs`)
- [OBS-12](observe.md#obs-12-the-collector-ships-every-containers-logs-to-cloud) The Collector ships every container's logs to Cloud
- [OBS-13](observe.md#obs-13-sealed-values-are-masked-before-lines-leave-the-server) Sealed values are masked before lines leave the Server
- [OBS-14](observe.md#obs-14-the-logs-page-names-servers-it-cant-read) The Logs page names servers it can't read
- [OBS-15](observe.md#obs-15-a-services-metrics-charts) A service's metrics charts
- [OBS-16](observe.md#obs-16-metrics-from-the-cli-ployz-metrics) Metrics from the CLI (`ployz metrics`)
- [OBS-17](observe.md#obs-17-a-servers-own-metrics-and-disk) A Server's own metrics and disk
- [OBS-19](observe.md#obs-19-canvas-badges-for-oom-and-crash-loops) Canvas badges for OOM and crash loops
- [OBS-20](observe.md#obs-20-the-collector-ships-metrics-every-10-seconds) The Collector ships metrics every 10 seconds
- [OBS-21](observe.md#obs-21-empty-metrics-say-why) Empty metrics say why
- [OBS-23](observe.md#obs-23-uptime-checks-right-after-a-deploy-and-on-demand) Uptime checks right after a deploy, and on demand
- [OBS-27](observe.md#obs-27-read-visitors-pages-and-sources-web) Read visitors, pages and sources (Web)
- [OBS-28](observe.md#obs-28-route-speed-and-failures-api-errors) Route speed and failures (API, Errors)
- [OBS-29](observe.md#obs-29-count-a-goal) Count a goal
- [OBS-30](observe.md#obs-30-request-rows-leave-the-server-without-the-visitors-address) Request rows leave the Server without the visitor's address
- [OBS-31](observe.md#obs-31-crashed-service-emails) Crashed-service emails
- [OBS-32](observe.md#obs-32-uptime-incident-emails) Uptime incident emails

## C13

**Self-hosted Cloud packaging.** Setup: a `--bare` Boat host with `.agents/skills/soak/scripts/selfhost/` (`setup https://HOST <version>`), GitHub apps for sign-in. Drive: verify-server (Bare hosts).

- [INS-19](install.md#ins-19-configure-and-start-self-hosted-ployz-cloud) Configure and start self-hosted Ployz Cloud
- [INS-20](install.md#ins-20-add-a-server-to-a-self-hosted-cloud) Add a server to a self-hosted Cloud
- [INS-21](install.md#ins-21-upgrade-self-hosted-cloud-and-refuse-an-older-image) Upgrade self-hosted Cloud, and refuse an older image
- [INS-22](install.md#ins-22-reseal-secrets-when-upgrading-self-hosted-cloud-from-v034-or-earlier) Reseal secrets when upgrading self-hosted Cloud from v0.3.4 or earlier
- [INS-23](install.md#ins-23-keep-logs-metrics-and-traces-in-self-hosted-cloud) Keep logs, metrics and traces in self-hosted Cloud
