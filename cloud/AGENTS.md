<!-- intent-skills:start -->
## Skill Loading

Before substantial work:
- Skill check: run `pnpm dlx @tanstack/intent@latest list`, or use skills already listed in context.
- Skill guidance: if one local skill clearly matches the task, run `pnpm dlx @tanstack/intent@latest load <package>#<skill>` and follow the returned `SKILL.md`.
- Monorepos: when working across packages, run the skill check from the workspace root and prefer the local skill for the package being changed.
- Multiple matches: prefer the most specific local skill for the package or concern you are changing; load additional skills only when the task spans multiple packages or concerns.
<!-- intent-skills:end -->


# Rules

- **No network requests in `beforeLoad` on client-side navigations.** `beforeLoad` is on the hot path of every navigation. Only the initial SSR load may hit the network. For subsequent navigations, the data must already be available client-side (e.g. via `createIsomorphicFn`, cached state, or a store).

## Route Data Loading

- Await only route-critical identity, redirect, and not-found work.
- Return noncritical readiness promises from loaders without awaiting them.
- Reveal consuming regions with `Await` or `Suspense`.

- **Before submitting or updating a PR, run `pnpm pr:check`.** This mirrors the PR CI checks (`pnpm typecheck`, `pnpm lint`, and `pnpm test`) so type errors, lint failures, and tests are caught locally before pushing or requesting review. If the CI workflow changes, update the `pr:check` script in `package.json` at the same time.
- **Inngest workflows that own durable rows must not leave ambiguous active state.** If an Inngest function creates or manages a row with statuses like `pending`/`running`, persist the Inngest `runId` on that row and handle `inngest/function.cancelled` so manual cancellation marks the row `cancelled` or another terminal status. Runtime cancellation may not undo remote side effects, but the cloud row must not remain active forever.
- **When editing app UI or changing shadcn component usage, you must use the `shadcn` skill first and follow its rules.** Prefer stock component composition, variants, and sizes before adding custom classes. Especially don't use custom text sizes, custom padding overrides like pb-2, and any colours like bg-primary/20.
- **`SidebarProvider` inside `WireframeSidebar` needs layout overrides.** The shadcn `SidebarProvider` renders a wrapper div with `flex min-h-svh w-full` which breaks Wireframe's fixed/absolute positioning system. Always override with `className="block h-full min-h-0"` (or similar) when nesting `SidebarProvider` inside a `WireframeSidebar` or `Wireframe`.




- **When I ask to update intents, use `npx @tanstack/intent@latest list` to inspect the current skills, then update the `intent-skills` block in `AGENTS.md` unless I ask for a different target file.** `npx @tanstack/intent@latest install` is guidance output here, not an automatic updater.
- **After updating intents in `AGENTS.md`, run `pnpm intents:sync`.** This runs `tessl skill review --optimize --yes` for each skill and then rewrites each `task`.
- **Use `npx @tanstack/intent@latest list` to inspect available skills when needed.** Use `--json` only if machine-readable output helps.
- **Use `npx @tanstack/intent@latest stale` only when I ask to check for outdated skill docs.**
- **Intent blocks should store the exact optimized skill description text.** In `intent-skills`, each `task` must match `validation.skillDescription` after the Tessl optimize workflow, and each `load` must use the exact listed path.
Adding an Environment Resource type starts in `environment-resource-types.ts`, then adds its strict snapshot/config parser, projection, and diff behavior to `environment-resource-node.ts`. Database constraints, collection projection, and canvas rendering still add their natural integration, but adapters must use the spine's type guard and config parser rather than re-enumerating resource types. Resource lifecycle diffs are resource-owned; anything that changes a service's container template (e.g. mounts) is a service-owned diff row so it is not double-counted.

We use react compiler - no need for memo/callback etc.

TanStack DB adds `$synced`, `$origin`, `$key`, and `$collectionId` to every final live-query row, including rows built with `.select(...)`. Pass whole live rows through `parseLiveQueryRow(schema, row)` from `#/lib/tanstack-db`; use `withoutVirtualProps(row)` when no schema parse is needed. Keep schemas strict—these helpers remove only TanStack's four virtual keys. The type-aware boundary test rejects direct `parse`/`safeParse` calls on whole live rows.
Never spread a live row into `insert` or `writeUpsert`; call `withoutVirtualProps(row)` first.

- **Use `useLiveSuspenseQuery` only when the route starts the backing preload.** Await it for route-wide pending, or return its readiness promise and gate the exact consumer with `Await`/`Suspense`. Construct derived collections only after raw readiness resolves. Ungated nested helpers, badges, autocomplete, and form panels should use `useLiveQuery` with an explicit loading state.

# Effect boundary conventions

- Model fallible application work as named `Effect` operations with capability-owned typed errors. Keep provider SDKs, SQL promises, and other throwing APIs behind `Effect.tryPromise` at their owning boundary.
- Provide database, authentication, configuration, Inngest, and provider clients through the shared application runtime. Do not create resources at import time or add mutable test bindings.
- Decode untrusted input with Effect Schema at the boundary. Server functions use the Actor middleware and the common public-error encoder; raw API routes use the shared HTTP public-error boundary.
- Inngest functions keep explicit triggers, steps, concurrency, and retry policy. Convert typed non-retriable failures to `NonRetriableError` only at the Inngest step boundary.
- Use `Effect.Result` only when a value representation is required by a protocol or callback. Prefer composing the `Effect` itself within application code.
