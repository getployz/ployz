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
  "modules/organization/members.queries.ts": { kind: "remote", freshness: "an Organization's members and pending invites, prefetched by Organization Settings and refetched after this tab's member writes; the change stream refetches it when a member or invite row changes. An invite link's Organization and state, prefetched by its page, which joining leaves" },
  "modules/agent/connect.queries.ts": { kind: "remote", freshness: "who is connecting an agent, their Organizations, and the agent asking, prefetched by /connect and never stale: the page is one authorization request, and answering it leaves" },
  "modules/organization/organization-state.queries.ts": { kind: "remote", freshness: "organization state: fresh on every mount; the change stream invalidates it when the organization changes" },
  "modules/server-upgrade/server-upgrade.queries.ts": { kind: "remote", freshness: "each Server's latest Upgrade attempt and when the latest success ended, latest-ofs over history computed on the server; the change stream refetches it when an attempt changes. The stable Release Channel pointer is cached five minutes here and in Cloud: releases are rare" },
  "modules/machines/server-drain.queries.ts": { kind: "remote", freshness: "each Server's latest Drain, the latest-of over history computed on the server; the change stream refetches it when a Drain's row changes" },
  "modules/service-act/service-act.queries.ts": { kind: "remote", freshness: "a Service's latest restart, start or stop and how many containers it moved; the change stream refetches it as each container's move is written" },
  "modules/machines/server-web-traffic.queries.ts": { kind: "remote", freshness: "each Server's latest web traffic change, the latest-of over history computed on the server; the change stream refetches it when a change's row changes" },
  "modules/volume-placement/volume-placement.queries.ts": { kind: "remote", freshness: "every Volume's mirrors and staged placement; the change stream refetches them when a row changes" },
  "modules/volume-copies/volume-copies.queries.ts": { kind: "remote", freshness: "a Volume's newest finished mirror round and its newest done one; the change stream refetches it when a run's row changes; a requested Refresh now is polled every 3s by volume-run.queries.ts, and its end refetches this by the volume-run key prefix. Mirror status polls every minute because the background sync writes it without a change to invalidate on" },
  "modules/volume-run/volume-run.queries.ts": { kind: "remote", freshness: "a Volume's last 50 runs, newest first; the change stream refetches them when a run's row changes" },
  "modules/billing/billing.queries.ts": { kind: "remote", freshness: "cached briefly; the subscription changes in Polar, not here" },
  "modules/github/github.queries.ts": { kind: "remote", freshness: "access fresh on mount because installs change in GitHub; install URL never changes; branches and file search cached briefly; build workflow readiness cached 30s, refetched on focus and polled while a workflow commit is awaited" },
  "modules/github/github-connect.queries.ts": { kind: "remote", freshness: "linked accounts fresh on mount because connects land through a GitHub redirect; an offer never changes, so it is read once" },
  "modules/github/github.collection.ts": { kind: "remote", freshness: "user repository cache: reused for a minute, polled while a picker is open so a requested sync appears; preloaded when a picker opens" },
  "modules/machines/namespace-cleanup.queries.ts": { kind: "remote", freshness: "keyed by the Namespaces the Servers report, so a new one is checked afresh; an Environment deleted elsewhere shows within a minute" },
  "components/agent-sidebar/approvals.queries.ts": { kind: "remote", freshness: "approvals waiting on a human, and each one a chat names, polled every 2s while pending: any CLI, tab or device can ask or answer, and no change stream carries them" },
  "components/agent-sidebar/threads.queries.ts": { kind: "remote", freshness: "the member's latest 30 chats, newest first, titled on the server; refetched when a turn in the open chat ends, the only writer of a member's own chats" },
  "modules/uptime/uptime.queries.ts": { kind: "remote", freshness: "each Environment's checks with their open incidents, streamed by the canvas loader; one Service's summary and its response time for the shown range, streamed by the drawer on the Monitoring tab and prefetched on hover of the tab or a range. Never awaited, so a slow read never holds the page. Stale after 30s and polled every minute because the checker writes without a Cloud write to invalidate on" },
  "modules/metrics/metrics.queries.ts": { kind: "remote", freshness: "one Service's or one Server's metrics for the shown range, read live from the Servers on each fetch. Stale after 30s and polled every minute because Servers sample without a Cloud write to invalidate on. A canvas card's OOM kills and restarts in the last hour read one bucket per Server, stale after a minute and polled every 5, because every card asks every Server" },
  "modules/notifications/notifications.queries.ts": { kind: "remote", freshness: "the signed-in user's email choices and whether this Cloud can send email, prefetched by the Account page; stale after a minute; only this user writes them, and each write shows at once and refetches" },
  "modules/account/account.collection.ts": { kind: "remote", freshness: "the signed-in user's live sessions and sign-in methods, prefetched by the Account page; stale after a minute; only this user revokes, and each revoke leaves at once" },
  "modules/marketing/marketing-site.queries.ts": { kind: "remote", freshness: "never stale: MARKETING_ORIGIN is fixed for the life of the server" },
  "modules/config-store/store-pr-grants.queries.ts": { kind: "remote", freshness: "installation permissions cached a minute: an owner approves them in GitHub, not here" },
} satisfies Record<string, { kind: DataSourceKind; freshness: string }>;
