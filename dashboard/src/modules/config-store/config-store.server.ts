import "@tanstack/react-start/server-only";
import { createRequire } from "node:module";
import type * as PloyzSdk from "@ployz/sdk";
import type { ConfigCommand, ConfigQuery, ConfigStore, ConfigView, ConfigWritten } from "@ployz/sdk";
import { sql } from "drizzle-orm";
import { Effect, Option, Redacted, Schema } from "effect";
import { GitCommand, gatherGitEvidence } from "#/modules/config-store/git-evidence.server";
import type { Actor } from "#/modules/identity/actor";
import { resolveCaller } from "#/modules/identity/caller.server";
import { getOrganizationForUserBySlug } from "#/modules/environment-design/workspace-repository.server";
import { storeChangeSources } from "#/modules/organization/change-log.sources";
import { AppConfig } from "#/server/config.server";
import { Database, type DatabaseService } from "#/server/database.server";
import { NotFound, Validation } from "#/server/public-error";
import type { StoreCall, StoreRefusal, StoreResult } from "./store.contract";

// SAFETY: the package exports this named CommonJS SDK surface at runtime.
const { openConfigStore, RpcError } = createRequire(import.meta.url)("@ployz/sdk") as Pick<
  typeof PloyzSdk,
  "openConfigStore" | "RpcError"
>;

/**
 * The Store creates its tables when it first opens, after Cloud's migrations have run, so Cloud attaches its
 * change log to them then. The lock serializes processes opening the Store at once.
 */
const attachChangeLog = Effect.gen(function* () {
  const database = yield* Database;
  yield* database.transaction(Effect.gen(function* () {
    const { drizzle } = yield* Database;
    yield* drizzle.execute(sql`select pg_advisory_xact_lock(1256)`);
    for (const [table, { key }] of Object.entries(storeChangeSources)) {
      const keys = sql.join(key.map((column) => sql`${column}::text`), sql`, `);
      yield* drizzle.execute(sql`
        select organization_change_attach(${table}::regclass, 'organization_id', variadic array[${keys}])
        where not exists (select from pg_trigger where tgrelid = ${table}::regclass and tgname = 'organization_change_insert')
      `);
    }
  }));
});

/** One Store handle per database for the process; a failed open is retried by the next call. */
const opened = new Map<string, Promise<ConfigStore>>();

/**
 * The Config Store in `database`, whose URL is `url`, with Cloud's change log attached to its tables. It seals secrets
 * with Cloud's encryption secret, so its ciphertext stays readable by Cloud's own sealing.
 */
export function storeAt(url: string, database: DatabaseService, sealingSecret: string) {
  let store = opened.get(url);
  if (store === undefined) {
    store = openConfigStore(url, sealingSecret).then(async (handle) => {
      await Effect.runPromise(attachChangeLog.pipe(Effect.provideService(Database, database)));
      return handle;
    });
    opened.set(url, store);
    store.catch(() => opened.delete(url));
  }
  return store;
}

/** A Store refusal travels to the CLI verbatim: the RPC error vocabulary, never the rejected value. */
function statusFor(code: string) {
  switch (code) {
    case "invalid_argument":
      return 422;
    case "not_found":
      return 404;
    case "conflict":
    case "ambiguous":
    case "confirmation_required":
      return 409;
    case "unauthenticated":
      return 401;
    case "unsupported":
      return 501;
    case "unavailable":
      return 503;
    default:
      return 500;
  }
}

function refusal(error: StoreRefusal) {
  const { code, message, details } = error;
  return Response.json({ error: { code, message, details } }, {
    status: statusFor(code),
    headers: { "cache-control": "no-store" },
  });
}

/**
 * One Store read or write as `organizationId`: the answer, or the Store's refusal verbatim. A write first gathers the
 * trusted GitHub evidence its repository Services need. Anything else (the Store failing to open, a broken binding)
 * is a defect.
 */
export const callStore = Effect.fn("ConfigStore.call")(function* (organizationId: string, call: StoreCall) {
  const config = yield* AppConfig;
  const database = yield* Database;
  const openStore = () => storeAt(config.database.url.href, database, Redacted.value(config.encryptionSecret));
  const read = (query: ConfigQuery) => openStore().then((store) => store.read(organizationId, query));
  const trusted = call.operation === "write"
    ? yield* gatherGitEvidence(organizationId, Option.getOrUndefined(Schema.decodeUnknownOption(GitCommand)(call.command)), read).pipe(
      Effect.catchTag("GithubObservationError", () => Effect.succeed(null)),
    )
    : undefined;
  if (trusted === null) {
    return { ok: false, refusal: { code: "unavailable", message: "GitHub didn't answer; retry.", details: null } } satisfies StoreResult<never>;
  }
  return yield* Effect.tryPromise({
    try: async (): Promise<StoreResult<ConfigView | ConfigWritten>> => {
      const store = await openStore();
      const value = call.operation === "read"
        ? await store.read(organizationId, call.query)
        : await store.write(organizationId, call.command, trusted);
      return { ok: true, value };
    },
    catch: (cause) => cause,
  }).pipe(Effect.catch((cause) => cause instanceof RpcError
    ? Effect.succeed<StoreResult<never>>({ ok: false, refusal: { code: cause.code, message: cause.message, details: cause.details } })
    : Effect.die(cause)));
});

/**
 * `POST /api/config/read` and `/api/config/write`: the Config Store's two public operations, as the calling
 * Organization. Nothing else of the Store is reachable over HTTPS.
 */
export const handleConfigRequest = Effect.fn("ConfigStore.handle")(function* (request: Request) {
  const config = yield* AppConfig;
  // TODO(#1275): dark in production until the Config Store cutover.
  if (config.nodeEnv === "production") return yield* new NotFound({ message: "Not found." });
  const operation = new URL(request.url).pathname.replace(/^\/api\/config\//, "");
  if (request.method !== "POST" || (operation !== "read" && operation !== "write")) {
    return yield* new NotFound({ message: "Not found." });
  }
  // Telemetry only: the CLI names a detected coding agent; nothing else reads it.
  yield* Effect.annotateCurrentSpan("ployz.agent", request.headers.get("x-ployz-agent") ?? "none");
  const caller = yield* resolveCaller(request.headers);
  const input: unknown = yield* Effect.tryPromise({
    try: () => request.json(),
    catch: () => new Validation({ message: "Expected a JSON body.", userFacing: true }),
  });
  // SAFETY: the Store decodes and validates the body itself, refusing anything else as invalid_argument.
  const call: StoreCall = operation === "read" ? { operation, query: input as ConfigQuery } : { operation, command: input as ConfigCommand };
  const result = yield* callStore(caller.organization.id, call);
  return result.ok ? Response.json(result.value, { headers: { "cache-control": "no-store" } }) : refusal(result.refusal);
});

/**
 * The dashboard's way into the Store: one read or write as `actor`, in the Organization named by `organizationSlug`
 * when the actor is a member of it (the same Organization gate as the Org Store's reads).
 */
export const callStoreAsMember = Effect.fn("ConfigStore.callAsMember")(function* (
  actor: Actor, organizationSlug: string, call: StoreCall,
) {
  const config = yield* AppConfig;
  // TODO(#1275): dark in production until the Config Store cutover.
  if (config.nodeEnv === "production") return yield* new NotFound({ message: "Not found." });
  const organization = yield* getOrganizationForUserBySlug(actor.userId, organizationSlug).pipe(Effect.orDie);
  if (!organization) return yield* new NotFound({ message: "Organization not found." });
  return yield* callStore(organization.id, call);
});
