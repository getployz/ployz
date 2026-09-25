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
  // ponytail: deployments and saved-state and node-config snapshots are unbounded history; move them to paged remote reads when orgs outgrow eager loading.
  "collections/collections.ts": { kind: "org-store", freshness: "table default for every table, deployments included: deployment events push progress" },
  "collections/org-store.ts": { kind: "org-store", freshness: "readiness only, once per organization; the change stream keeps tables fresh" },
  "modules/environment-design/environment-document.collection.ts": { kind: "org-store", freshness: "derived from environments and projects" },
  "modules/environment-design/resource.collection.ts": { kind: "org-store", freshness: "derived from resources, lineages, positions, and documents" },
  "modules/services/services.collection.ts": { kind: "org-store", freshness: "derived from services and documents" },
  "modules/deployments/deployment.collection.ts": { kind: "org-store", freshness: "derived from deployments, snapshots, and volume removals" },
  "modules/deployments/environment-change-state.queries.ts": { kind: "org-store", freshness: "server projection, refetched when deployment or saved-state metadata changes" },
  "modules/runtime/runtime.collection.ts": { kind: "runtime", freshness: "SSE runtime watch" },
  "modules/runtime/container-log.stream.ts": { kind: "runtime", freshness: "SSE log stream, older pages on scroll" },
  "modules/environment-design/workspace.queries.ts": { kind: "remote", freshness: "organization state: fresh on every mount; the change stream invalidates it when the organization changes" },
  "modules/billing/billing.queries.ts": { kind: "remote", freshness: "cached briefly; the subscription changes in Polar, not here" },
  "modules/github/github.queries.ts": { kind: "remote", freshness: "access fresh on mount because installs change in GitHub; install URL never changes; branches and file search cached briefly; build workflow readiness cached 30s, refetched on focus and polled while a workflow commit is awaited" },
  "modules/github/github.collection.ts": { kind: "remote", freshness: "user repository cache: reused for a minute, polled while a picker is open so a requested sync appears; preloaded when a picker opens" },
  "modules/runtime/teardown.queries.ts": { kind: "remote", freshness: "fresh on mount; polls while an attempt is busy" },
  "modules/runtime/volume-removal.queries.ts": { kind: "remote", freshness: "fresh on mount; polls while an attempt is busy" },
  "modules/deployments/deployment-log.collection.ts": { kind: "remote", freshness: "polls until the deployment finishes; a finished log is never refetched" },
  "modules/deployments/deployment-build-log.queries.ts": { kind: "remote", freshness: "fresh on mount; polls until the build finishes" },
} satisfies Record<string, { kind: DataSourceKind; freshness: string }>;
