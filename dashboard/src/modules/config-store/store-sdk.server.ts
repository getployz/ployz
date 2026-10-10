import "@tanstack/react-start/server-only";
import { createRequire } from "node:module";
import type * as PloyzSdk from "@ployz/sdk";
import type { ConfigStore } from "@ployz/sdk";
import { sql } from "drizzle-orm";
import { Context, Data, Effect, Layer, Redacted, Semaphore } from "effect";
import { storeChangeSources } from "#/modules/organization/change-log.sources";
import { AppConfig } from "#/server/config.server";
import { Database, type DatabaseService } from "#/server/database.server";
import { StoreRefused } from "./store.contract";

// SAFETY: the package exports this named CommonJS SDK surface at runtime.
const { openConfigStore, RpcError } = createRequire(import.meta.url)("@ployz/sdk") as Pick<
  typeof PloyzSdk,
  "openConfigStore" | "RpcError"
>;


/** Whether the Store refused with `code`. */
export const refusedWith = (code: string) => <E>(error: E) => error instanceof StoreRefused && error.code === code;

/**
 * A call into the Config Store's native SDK, as an Effect failing with its refusal. The one place the Store's
 * promises become Effects: anything else it rejects with (a broken binding) is a defect.
 */
export const storeTry = <A>(call: () => Promise<A>): Effect.Effect<A, StoreRefused> =>
  Effect.tryPromise({ try: call, catch: (cause) => cause }).pipe(
    Effect.catch((cause) => cause instanceof RpcError
      ? Effect.fail(new StoreRefused({ code: cause.code, message: cause.message, details: cause.details }))
      : Effect.die(cause)),
  );

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

export class ConfigStoreOpenFailure extends Data.TaggedError("ConfigStoreOpenFailure")<{ readonly cause: unknown }> {
  readonly publicErrorCategory = "internal" as const;
}

/** The open that converted Conditional Syncs to offers logs what it did, once. */
const logConverted = (store: ConfigStore) => {
  const converted = store.converted();
  return converted === null ? Effect.void : Effect.logInfo("Conditional Syncs converted to offers.", converted);
};

/**
 * The Config Store in `database`, whose URL is `url`, with Cloud's change log attached to its tables. It seals secrets
 * with Cloud's encryption secret, so its ciphertext stays readable by Cloud's own sealing.
 */
export const openCloudStore = (url: string, database: DatabaseService, sealingSecret: string) =>
  Effect.tryPromise({
    try: () => openConfigStore(url, sealingSecret),
    catch: (cause) => new ConfigStoreOpenFailure({ cause }),
  }).pipe(Effect.tap(logConverted), Effect.tap(() => attachChangeLog.pipe(
    Effect.provideService(Database, database),
    Effect.mapError((cause) => new ConfigStoreOpenFailure({ cause })),
  )));

/** Cloud's Config Store, in Cloud's database: opened on first use, once; a failed open is retried by the next. */
export class CloudStore extends Context.Service<CloudStore, {
  readonly open: Effect.Effect<ConfigStore, ConfigStoreOpenFailure>;
}>()("ployz/CloudStore") {}

export const CloudStoreLive = Layer.effect(CloudStore, Effect.gen(function* () {
  const config = yield* AppConfig;
  const database = yield* Database;
  const lock = yield* Semaphore.make(1);
  let opened: ConfigStore | undefined;
  const open = openCloudStore(config.database.url.href, database, Redacted.value(config.encryptionSecret)).pipe(
    Effect.tap((store) => Effect.sync(() => { opened = store; })),
  );
  return {
    open: lock.withPermits(1)(Effect.suspend(() => opened === undefined ? open : Effect.succeed(opened))),
  };
}));

/** Cloud's Config Store. */
export const cloudStore = Effect.gen(function* () {
  return yield* (yield* CloudStore).open;
});
