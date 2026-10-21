- `scripts/verify/up.sh` runs a seeded, signed-in dashboard for this checkout, even a fresh worktree. Verify UI in a browser with the `verify-dashboard` skill.
- Locally, run `pnpm typecheck`, `pnpm lint` and `pnpm exec vitest run <changed test files>`. Keep `pnpm pr:check` aligned with the PR CI checks.

<!-- intent-skills:start -->
## Skill Loading

When framework guidance is needed:
- Use matching skills already listed in context first.
- Discover missing guidance with `pnpm dlx @tanstack/intent@latest list` from the affected package root (`dashboard/` here), then load the matching `<package>#<skill>`.
- Multiple matches: prefer the most specific local skill for the package or concern you are changing; load additional skills only when the task spans multiple packages or concerns.
<!-- intent-skills:end -->

To update intents, edit the block above after `pnpm dlx @tanstack/intent@latest list`. `install` only prints guidance.

# Rules

- Reading data, changing loaders, or adding pending UI follows [CODING_STANDARDS.md](CODING_STANDARDS.md). Client navigations never hit the network in `beforeLoad`.
- React Compiler is on. Don't add `useMemo` or `useCallback`.
- shadcn changes go through the `shadcn` skill first. Use stock composition, variants and sizes. No custom text sizes, padding overrides, or colours.
- An Inngest function that owns rows with active statuses persists its `runId` and marks the row terminal on `inngest/function.cancelled`. Build it with `attemptLifecycle` (`src/modules/inngest/attempt-lifecycle.ts`).
- A new Environment Resource type starts in `environment-resource-types.ts`, then its config parser in `environment-resource-node.ts` and its lifecycle diff in `environment-change-set.ts`. Adapters use that parser rather than re-enumerating types. A change to a service's container template (e.g. mounts) is a service-owned diff row.
- TanStack DB adds `$synced`, `$origin`, `$key` and `$collectionId` to every live-query row, `.select(...)` rows included. Parse whole rows with `parseLiveQueryRow(schema, row)` from `#/lib/tanstack-db`, and strip them with `withoutVirtualProps(row)` before `insert` or `writeUpsert`.

# Effect

- Fallible work is a named `Effect` with capability-owned typed errors. Throwing APIs sit behind `Effect.tryPromise` at their owning boundary.
- Database, auth, config, Inngest and provider clients come from the shared application runtime. No resources at import time, no mutable test bindings.
- Decode untrusted input with Effect Schema at the boundary. Server functions use the Actor middleware and the common public-error encoder. Raw API routes use the shared HTTP public-error boundary.
- Inngest functions keep explicit triggers, steps, concurrency and retry policy. Convert typed non-retriable failures to `NonRetriableError` only at the step boundary.
- Use `Effect.Result` only when a protocol or callback needs a value. Otherwise compose the `Effect`.
