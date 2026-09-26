# Dashboard coding standards

Apply these rules whenever you read data, write a route loader, or add pending UI. Product design lives in [DESIGN.md](DESIGN.md). Rules marked ✓ are checked by `src/collections/data-boundaries.static.test.ts` and `src/collections/collections.test.ts`; guidance under **Keep** is for review.

## Data: three kinds, one door

```
                 ┌─ data files: src/collections/, *.collection.ts, *.queries.ts, *.stream.ts ─┐
 Postgres rows ──┤ Org Store     every org table, org-wide, eager, one readiness gate          │
 core runtime  ──┤ Runtime       SSE into local collections                                    │
 anything else ──┤ Remote Read   Query with an explicit staleTime                              │
                 └──────────────────────────────────────────────────────────────────────────────┘
                                   ▲ hooks and collection getters
 routes + components ──────────────┘  never fetch, never await data
```

Pick the kind with one question each:

1. A Cloud-owned row with a bounded count per organization? **Org Store.** Bounded means it grows only with things the user creates and keeps (projects, environments, services, volumes, servers). Rows that grow when the user clicks Deploy or Save, or simply with time, are history. A fixed slice of history (the latest N per environment) is bounded; the rest is not.
2. Core runtime state? **Runtime.**
3. Anything else: third-party APIs, unbounded history, logs, on-demand searches? **Remote Read.**

Ask the question of what a view derives from, not just what it shows. A bounded view (a deployment list's status, each volume's latest config) that joins an unbounded table pulls that whole table into the Org Store. Compute such facts on the server instead, as a column or a server projection.

File names say where a source lives, not its kind: a Query file that projects org rows on the server (`environment-change-state.queries.ts`) is still Org Store. The registry records each file's kind.

### Rules

1. ✓ **One door.** Only data files create collections, call `queryOptions`, or write a `queryFn`. Register each such file with its kind and freshness in `src/collections/data-sources.ts`; the test enforces a complete list, and review checks the kind and freshness text. Components use the hooks and getters those files export. A component may call a read server function only as one step of a user command (a data-loss preview before confirming, waiting on a removal it started); list the file with its reason in the boundary test. Raw `fetch` and `EventSource` belong in data files or server code; exceptions go in the boundary test's network allowlist with a reason.
2. ✓ **The Org Store is org-wide.** Collection keys hold session, user, and organization only (`CollectionScope` has no environment). Select one environment or project inside a live query with `.where(...)`. Adding an Org Store table means adding its getter to `orgStoreTables` in `src/collections/collections.ts`, which the gate preloads and the change stream refetches.
3. ✓ **One gate.** The organization layout starts the Org Store with `prefetchOrgStore`. `DashboardShell` gates its content region once with `useOrgStoreGate`. Everything below that gate may read Org Store rows with `useLiveSuspenseQuery` or collection getters without its own loader or boundary. Adding a derived view or server projection means adding it to `orgStoreViews` or `orgStoreProjections`. **Org Store views derive only from bounded rows:** a count, a latest-of, or a per-attempt status computed from history comes from the server. On the client the gate shows a retryable error; during SSR an Org Store failure fails the organization route.
4. ✓ **Loaders only decide and prefetch.** Loaders and `beforeLoad` await only `require*` helpers (access, not-found, redirect) and `prefetch*` helpers from `src/collections/route-data.ts`, plus the in-memory `getAuthSession`. `require*` awaits on server and client. `prefetch*` awaits during SSR so HTML is complete, and never blocks client navigation. `prefetchOrgStore` fails the route on an SSR error; `prefetchRemote` leaves the error to the page's boundary.
5. ✓ **Remote Reads declare freshness.** Every `queryOptions`, and every options object with a `queryFn`, sets `staleTime`; only Org Store tables in `src/collections/` inherit the collection `staleTime` default.
6. **Freshness is pushed.** Runtime pushes over its SSE stream. Org Store tables are pushed by the Organization change log: one stream per tab names the changed collections, and each reads only the rows changed since its cursor. They also refetch on focus and reconnect, run no timer, and land their own writes through `writeCommitted`. A Remote Read that polls says why in the registry.
7. ✓ **Spinners mean a write or a running process.** Reads show prefetched content, a skeleton, or nothing. New spinner locations go in the allowlist in the boundary test with a reason.
8. ✓ **Loaders preload the page.** A page's loader prefetches every Remote Read the page shows (`prefetchRemote`), so navigation lands on data already in memory. Reads that depend on a user action (a picked repository, a search, opening logs) are listed as on demand in the boundary test with what warms them instead. The test checks that some loader prefetches each Remote Read factory; review checks it is the page's own loader. Prefetch a page's reads in one `prefetchRemote` call so they start together.
9. ✓ **Writes are optimistic.** A write applies to the in-memory rows at once, saves in the background, and on failure rolls back and toasts. Pages and components never wait for a save: no `await` on a server call or `isPersisted`. The only exceptions are commands listed in the boundary test with a reason: the server assigns the new row's id, the action deploys or is destructive, or it involves money or an external service.
10. ✓ **Environment documents change through `editEnvironmentDocument`.** It applies the edit, queues saves per environment against the latest revision (the server rejects stale ones), and owns the failure toast. Commands that must save against the latest revision without an up-front change (discard) use `useEnvironmentDocumentQueue().enqueue`; commands that read the saved state (publish, destructive review) first `await settled(environmentId)`. Only listed commands that receive a whole new document write it directly.
11. ✓ **One shell.** Only the organization layout renders `DashboardShell`, so navigation within an organization never remounts or hides it. The one exception is the full-screen project creation flow.

### Keep

- Read a prefetched Remote Read with `useSuspenseQuery` (the route's `pendingComponent` or a local `Suspense` covers only that page) or `useQuery` for reads that poll or only matter after a user action.
- Keep confirm dialogs for irreversible actions (seal, delete), but close them as soon as the user confirms; the optimistic write does the rest.
- Warm reads the user is about to need on intent (menu open, hover, focus), and start independent reads together (`useSuspenseQueries`, not sequential `useSuspenseQuery` calls). Links need nothing extra: the router preloads every visible link's loader (`defaultPreload: "viewport"`).
- Chrome above the gate (sidebar, header) uses `useLiveQuery` or `useOrgStoreStatus` and handles pending state. Project creation (`_project/new`) renders outside the shell and its gate, so it must not use `useLiveSuspenseQuery`.
- Match an environment with `findEnvironment` (project slug and namespace) or by id; a namespace alone is ambiguous across projects.
- Preserve failures. Loading and failed reads are not empty collections: the Org Store gate shows a retryable error; Remote Reads expose Query's error state.
- Collection rows belong to DB serialization; do not duplicate them in loader payloads or Query dehydration.
- Keep version calculation and readiness inside the owning data module. Test the SSR-to-hydration handoff for loading fallbacks as well as mismatched HTML.

Verify cold SSR waits, cold client navigation commits with a pending region, readiness reveals content, and failures reach an error state. Warm navigation reuses cached data.
