import "@tanstack/react-start/server-only";
import { type ConfigStore, openConfigStore, RpcError } from "@ployz/sdk";
import { type SQL, sql } from "drizzle-orm";
import { Context, Data, Effect, Layer, Redacted, Semaphore } from "effect";
import { clusterExpectation, loadOrganizationConnections, logForeignServers } from "#/modules/runtime/connections.server";
import { uptimeForgetServicesTrigger } from "#/modules/uptime/tables";
import { analyticsForgetEnvironmentsTrigger } from "#/modules/web-analytics/tables";
import { storeChangeSources } from "./store.change-sources";
import { ADVISORY_LOCK } from "#/server/advisory-locks";
import { AppConfig } from "#/server/config.server";
import { Database, type DatabaseService } from "#/server/database.server";
import { StoreRefused } from "./store.contract";


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
 * The Organization's Servers, to dial, with the Cluster the dial expects them in: its pairing's recorded Sealing Key,
 * else none. No Servers while it has no pairing.
 */
export const deployDialOf = Effect.fn("ConfigStore.deployDialOf")(function* (organizationId: string) {
  const loaded = yield* loadOrganizationConnections(organizationId);
  return loaded.kind === "ready"
    ? { connections: loaded.connections, expect: clusterExpectation(loaded) }
    : { connections: [], expect: null };
});

/** A Store call that dials the Organization's Servers (`deployDialOf`). */
export const dialedStore = <A>(organizationId: string, call: (dial: Effect.Success<ReturnType<typeof deployDialOf>>) => Promise<A>) =>
  Effect.flatMap(deployDialOf(organizationId), (dial) => storeTry(() => call(dial)));

/** `dialedStore` for a call that answers the Servers it left because they run in another Cluster, which it logs. */
export const withDeployDial = <A extends { readonly foreign: readonly string[] }>(
  organizationId: string, call: (dial: Effect.Success<ReturnType<typeof deployDialOf>>) => Promise<A>,
) => Effect.tap(dialedStore(organizationId, call), ({ foreign }) => logForeignServers(organizationId, foreign));

/** Each module's trigger that forgets what it keeps for a Store row once that row is deleted. */
const FORGET_TRIGGERS: readonly {
  readonly table: string; readonly name: string; readonly ifMissing: boolean; readonly create: SQL;
}[] = [uptimeForgetServicesTrigger, analyticsForgetEnvironmentsTrigger];

/**
 * The Store creates its tables when it first opens, after Cloud's migrations have run, so Cloud attaches its
 * change log to them then, and the modules' triggers that forget what they keep for a deleted Store row. The lock
 * serializes processes opening the Store at once.
 */
const attachChangeLog = Effect.gen(function* () {
  const database = yield* Database;
  yield* database.transaction(Effect.gen(function* () {
    const { drizzle } = yield* Database;
    yield* drizzle.execute(sql`select pg_advisory_xact_lock(${ADVISORY_LOCK.storeChangeLog})`);
    for (const [table, { key }] of Object.entries(storeChangeSources)) {
      const keys = sql.join(key.map((column) => sql`${column}::text`), sql`, `);
      yield* drizzle.execute(sql`
        select organization_change_attach(${table}::regclass, 'organization_id', variadic array[${keys}])
        where not exists (select from pg_trigger where tgrelid = ${table}::regclass and tgname = 'organization_change_insert')
      `);
    }
    for (const trigger of FORGET_TRIGGERS) {
      if (trigger.ifMissing) {
        const [existing] = yield* drizzle.execute<{ exists: boolean }>(sql`
          select exists (select from pg_trigger where tgrelid = ${trigger.table}::regclass and tgname = ${trigger.name}) as exists
        `, "objects");
        if (existing?.exists === true) continue;
      }
      yield* drizzle.execute(trigger.create);
    }
  }));
});

export class ConfigStoreOpenFailure extends Data.TaggedError("ConfigStoreOpenFailure")<{ readonly cause: unknown }> {
  readonly publicErrorCategory = "internal" as const;
}

/** The open that retired Conditional Syncs logs what it did, once. */
const logConverted = (store: ConfigStore) => {
  const converted = store.converted();
  return converted === null ? Effect.void : Effect.logInfo("Conditional Syncs retired.", converted);
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
