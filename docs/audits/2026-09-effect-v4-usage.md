# Effect v4 usage audit (dashboard)

Audit of `dashboard/src` against `effect@4.0.0-rc.112`, `@effect/sql-pg@4.0.0-rc.112`,
`@effect/vitest@4.0.0-rc.112`. Six areas were reviewed in parallel (services and
layers, errors, Schema, composition and concurrency, tests, database and Inngest
boundaries); every API claim below was checked against the installed `.d.ts` files.

Two findings from the first pass were retracted after verification and are
recorded here so they are not re-raised:

- The `retriable` field on domain errors is a deliberate, pervasive convention
  (30+ sites) read through `parseErrorEvidence`, not an orphan.
- `cause.name === "NonRetriableError"` in `environment-deployment.inngest.ts` is
  required: Inngest rethrows a step's exhausted failure as `StepError`, which
  keeps only the serialized `name`.
- Ployz SDK methods other than `connect`, `confirm`, and `watch` do not accept an
  `AbortSignal`, so not threading one there is not a defect.
- `@effect/sql-pg`'s `PgClient.listen` is not a drop-in for the hand-rolled
  `subscribeDatabaseNotifications`: it registers a no-op `error` handler on its
  dedicated client and never fails the stream on connection end, so a dropped
  backend would go undetected. The hand-rolled version fails on `error`/`end`,
  which is what makes reconnect possible. It stays.
- `pg_notify` in `pairing-removal.server.ts` runs inside the transaction on
  purpose: transactional NOTIFY fires on commit.
- `Schema.is` takes no parse options, so it cannot replace the guards that
  reject excess properties (`isValid`, the two GitHub struct guards). Only the
  three option-free scalar guards were converted.
- The `step.run` around the envelope decode in
  `environment-deployment.inngest.ts` is a tested contract (the engine test
  asserts one `step.run` call for the decode). It stays.
- Internal Inngest event payloads are not unvalidated: every consumer decodes
  its envelope with a `Schema.Struct` at the handler (deploy, volume and
  machine removal, teardown, GitHub sync and ingestion, billing sync,
  cancellation). Only the `eventType` declarations in `inngest/events.ts` are
  type-only, and the per-consumer schemas are duplicated rather than shared.
  That is a consolidation, not a bug, and was not pursued.

## Top five

### 1. Effect to Inngest error boundary (fixed in this PR)

`src/server/run.server.ts`

- `makeEffectRunner` threw the raw `Cause` whenever the cause tree carried a
  defect or interruption, even when a typed failure was present. A `Conflict`
  racing a finalizer defect lost its public-error category and its
  non-retriable classification.
- `makeInngestEffectRunner` could not classify a raw `Cause` (it is not an
  `Error`), so every defect surfaced to Inngest as `"Effect activity failed."`
  and was retried. The `NonRetriableError` branch dropped `cause` for
  non-`Error` values.

Fix: a typed failure wins (matching `Cause.squash` precedence) and the shadowed
defect is logged; classification and messages use the squashed cause; `cause`
is always forwarded.

### 2. Pairing-removal listener latches dead (fixed in a follow-up PR)

`src/modules/runtime/organization-runtime.server.ts:94-104`

- On any stream failure `listenerFailure` is set and never cleared, so every
  later `OrganizationRuntime.open` fails until the process restarts.
  `database.server.postgres.test.ts` shows `pg_terminate_backend` produces
  exactly that failure.

Fix: the listener re-subscribes with exponential, jittered backoff capped at
30 seconds. While it is down, `open` fails closed and live sessions are
closed, since removals during the gap were not observed. The first LISTEN
still gates layer startup. Malformed payloads are logged and skipped instead
of ending the listener. The database-layer subscription is unchanged (see the
retraction above).

### 3. Runtime escape hatches and lost interruption (fixed in a follow-up PR)

- `src/modules/deployments/runtime-activities.server.ts:233` runs a bare
  `Effect.runPromise` with a manually provided `Database` inside the SDK
  `onEvent` callback, while already executing under `AppRuntime`. Plan: a
  bounded `Queue` fed by `Queue.offerUnsafe` from the callback and drained by a
  `forkScoped` child fiber, so context, spans, and interruption are inherited.
- `src/server/auth.server.ts:153-162` `runHook` is a second hand-built runtime
  for better-auth hooks. Plan: call `runAppEffect`, or capture
  `Effect.context()` once and `Effect.provideContext` if there is an
  initialization-order cycle.
- `src/routes/api/runtime/events.ts:35` calls `runAppEffect` without
  `{ signal: request.signal }`, unlike every boundary in `tanstack.ts`.
- `src/modules/github/github-observation.api.ts:434,511` ignore the
  `AbortSignal` that `Effect.tryPromise` passes to the thunk.
- `AppRuntime.dispose()` is never called outside a test fixture; there is no
  SIGTERM hook, so the pool's `acquireRelease` finalizer never runs in
  production.

### 4. No ceiling on the organization connect phase (fixed in a follow-up PR)

- `OrganizationRuntime.open` had no bound on establishing a shared machine
  session. The SDK's `timeoutMs` bounds the whole session lifetime, which
  would cut long deploys, so only the connect phase is bounded (30 seconds,
  `ORGANIZATION_CONNECT_TIMEOUT`). A timeout surfaces as `unreachable`.
- `preview`/`inspect` take no `AbortSignal`, so an Effect timeout there would
  only orphan the promise; interactive callers are bounded by the request
  signal and the session scope closing.
- The original first bullet (a remote call inside a row-locked transaction)
  is retracted; see above.

### 5. Test conventions (partially fixed in a follow-up PR)

- 18 of about 60 Effect-touching test files use `@effect/vitest`; 17 of 28
  `*.postgres.test.ts` files hand-roll `ManagedRuntime.make` plus
  `beforeAll`/`afterAll`.
- 32 sites assert `Exit.isFailure` and dig into `exit.cause` manually; the
  `assertExitFailure`/`assertFailure`/`assertInstanceOf` helpers in
  `@effect/vitest/utils` are unused. `config.server.effect.test.ts:46,82,97`
  do not check which error fired.
- Two near-identical postgres docker harnesses exist: `src/test/postgres.ts`
  (Effect-native) and `github-ingestion.postgres-test-harness.ts`
  (promise-based, used by 17 files, most outside GitHub).
- `TestClock` is unused despite `Effect.sleep` retry loops in
  `pairing-removal.server.ts:173` and `runtime-activities.server.ts:217`.
- `Layer.mock` and `it.scoped` do not exist in rc.112; the codebase correctly
  never uses them.

Done: the `Exit.isFailure` digs in unit tests use `Effect.flip` plus an
`instanceof` assertion, and a static-analysis test keeps the idiom out.
Remaining, since they need a Postgres container to verify: migrate the
`*.postgres.test.ts` files to `it.layer`, consolidate the two docker
harnesses, and add `TestClock` tests for the two `Effect.sleep` loops.

## Smaller items

Done in a follow-up PR: the GitHub token `Cache`, `Schema.is` for the scalar
guards, logging of dropped dispatch failures, the duplicate
`isUniqueViolation`, and removal of the unused `parseEnvironmentResourceNodeConfig`.
Still open: branded identifiers (large and mechanical; best done one
identifier at a time once the current PRs merge) and `publicMessage` on public
errors (a product copy decision, not a code defect).

- `Uuid` is an unbranded string reused for every identifier in 15+ files;
  `Schema.brand` is used once. Brand one identifier at a time and follow the
  type errors.
- `Schema.is(schema)` replaces `isValid` in `environment-design/schema.ts` and
  the five `isValidGithub*` guards in `github-ingestion.contracts.ts`.
- `environment-resource-node.ts:89-95` builds a typed `SchemaError` Effect then
  `Effect.runSync`s it into a throw; return the Effect.
- `encodePublicError` replaces every domain message with one of six fixed
  strings. Add an opt-in `publicMessage` on tagged errors.
- `github-ingestion.branch.repository.ts:538-548` drops every non-SQL dispatch
  failure and every defect silently; add `Effect.tapErrorCause` logging and
  re-fail on `Conflict`.
- The GitHub installation-token cache is a mutable `Map` without in-flight
  deduplication; `Cache.make({ lookup, capacity, timeToLive })` deduplicates
  concurrent misses.
- `workspace-operations.server.ts:35-37` duplicates `isUniqueViolation` from
  `database.server.ts`.
- `environment-deployment.inngest.ts:433-437` wraps a pure envelope decode in
  `step.run`, costing a step round-trip for no durability.

## What is already right

- Layer memoization in `runtime.server.ts` is correct: `InfrastructureLive` is
  shared by object identity between the auth and runtime branches, so each
  leaf layer is built once.
- `Database.transaction` swaps the service correctly at every call site
  checked; callers re-acquire `Database` inside the transaction body.
- `Effect.fn` at 336 sites versus 109 `Effect.gen`; all 27 `Effect.tryPromise`
  boundaries produce tagged errors, enforced by
  `effect-try-promise-boundary.test.ts`.
- `Context.Service` is the current API in this rc (there is no `ServiceMap`
  module and no `Effect.Service`).
- `organization-runtime.server.ts` session scoping (`Scope.fork`, `Deferred`
  cancellation raced against connect, finalizers) is the pattern the rest of
  the codebase should copy.
