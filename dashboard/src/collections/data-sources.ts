/**
 * Every file that creates a dashboard data source. `data-boundaries.static.test.ts`
 * fails when a file creates a collection or Query read without an entry here.
 * The test guarantees the list is complete. `kind` and `freshness` are reviewed documentation:
 * they explain the policy and why, while the code is authoritative.
 *
 * - `org-store`: Cloud-owned rows for one organization, loaded eagerly and org-wide,
 *   ready behind the shell's single content gate, read with live queries.
 * - `runtime`: core runtime testimony pushed over SSE into local collections.
 * - `remote`: third-party, unbounded, or on-demand reads through Query, with explicit `staleTime`.
 */
export type DataSourceKind = "org-store" | "runtime" | "remote";

export const dataSources = {
  "collections/query-collection.ts": { kind: "org-store", freshness: "table default: the Organization change stream pushes which tables changed and each reads rows changed since its cursor; no timer; refetch on focus and reconnect; land this user's writes via writeCommitted" },
  "collections/collections.ts": { kind: "org-store", freshness: "table default for every table" },
  "collections/org-store.ts": { kind: "org-store", freshness: "readiness only, once per organization; the change stream keeps tables fresh" },
  "modules/config-store/store-view.queries.ts": { kind: "remote", freshness: "one bounded Config Store view per query (an Environment's Settings, its diff or plan, a Deployments page, one Deployment), prefetched by loaders; the change stream refetches the views each Store table family backs; a domains view polls every 30s while a domain is setting up or needs DNS (certificates and DNS change without a Store write); this tab's pending edits show over it until they commit" },
  "modules/runtime/runtime.collection.ts": { kind: "runtime", freshness: "SSE runtime watch" },
  "modules/runtime/container-log.stream.ts": { kind: "runtime", freshness: "SSE log stream, older pages on scroll" },
  "modules/organization/organization-state.queries.ts": { kind: "remote", freshness: "organization state: fresh on every mount; the change stream invalidates it when the organization changes" },
  "modules/server-upgrade/server-upgrade.queries.ts": { kind: "remote", freshness: "each Server's latest Upgrade attempt and when the latest success ended, latest-ofs over history computed on the server; the change stream refetches it when an attempt changes. The stable Release Channel pointer is cached five minutes here and in Cloud: releases are rare" },
  "modules/machines/server-drain.queries.ts": { kind: "remote", freshness: "each Server's latest Drain, the latest-of over history computed on the server; the change stream refetches it when a Drain's row changes" },
  "modules/billing/billing.queries.ts": { kind: "remote", freshness: "cached briefly; the subscription and the Custom Domain Capability change in Polar, not here, and a completed checkout syncs them at once" },
  "modules/github/github.queries.ts": { kind: "remote", freshness: "access fresh on mount because installs change in GitHub; install URL never changes; branches and file search cached briefly; build workflow readiness cached 30s, refetched on focus and polled while a workflow commit is awaited" },
  "modules/github/github.collection.ts": { kind: "remote", freshness: "user repository cache: reused for a minute, polled while a picker is open so a requested sync appears; preloaded when a picker opens" },
  "modules/machines/namespace-cleanup.queries.ts": { kind: "remote", freshness: "keyed by the Namespaces the Servers report, so a new one is checked afresh; an Environment deleted elsewhere shows within a minute" },
  "modules/config-store/store-pr-grants.queries.ts": { kind: "remote", freshness: "installation permissions cached a minute: an owner approves them in GitHub, not here" },
} satisfies Record<string, { kind: DataSourceKind; freshness: string }>;
