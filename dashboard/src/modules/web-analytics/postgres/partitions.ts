import { sql } from "drizzle-orm";
import { Effect } from "effect";
import { Client, DatabaseError } from "pg";
import { retentionCutoff } from "#/modules/web-analytics/local-time";
import { AppConfig } from "#/server/config.server";
import { Database, sqlErrorFrom } from "#/server/database.server";

const DAY_MS = 86_400_000;
const DAYS_AHEAD = 2;

/** The UTC day holding `date`, as YYYY-MM-DD. Request partitions split on UTC midnight. */
export const dayOf = (date: Date) => date.toISOString().slice(0, 10);

const nextDay = (day: string) => dayOf(new Date(Date.parse(`${day}T00:00:00Z`) + DAY_MS));

export const partitionName = (day: string) => `request_${day.replaceAll("-", "")}`;

const DUPLICATE_TABLE = "42P07";
const UNIQUE_VIOLATION = "23505";
const LOCK_NOT_AVAILABLE = "55P03";
const DDL_LOCK_TIMEOUT_MS = 5000;

const pgCode = (cause: unknown) => {
  const driverError = sqlErrorFrom(cause)?.reason.cause;
  return driverError instanceof DatabaseError ? driverError.code : undefined;
};

const ensured = new Set<string>();

export const ensurePartitions = Effect.fn("ensurePartitions")(function* (days: ReadonlyArray<string>) {
  const missing = [...new Set(days)].filter((day) => !ensured.has(day));
  if (missing.length === 0) return;
  const database = yield* Database;
  for (const day of missing) {
    yield* database.transaction(Effect.gen(function* () {
      const { drizzle } = yield* Database;
      yield* drizzle.execute(sql.raw(`set local lock_timeout = '${DDL_LOCK_TIMEOUT_MS}ms'`));
      yield* drizzle.execute(sql.raw(
        `create table if not exists analytics.${partitionName(day)} partition of analytics.request `
        + `for values from ('${day} 00:00+00') to ('${nextDay(day)} 00:00+00')`,
      ));
    })).pipe(Effect.catchIf(
      (error) => pgCode(error) === DUPLICATE_TABLE || pgCode(error) === UNIQUE_VIOLATION,
      () => Effect.void,
    ));
    ensured.add(day);
  }
});

export const forgetPartitions = () => ensured.clear();

export const ensureComingPartitions = Effect.fn("ensureComingPartitions")(function* (now: Date) {
  yield* ensurePartitions(Array.from({ length: DAYS_AHEAD + 1 }, (_, ahead) => dayOf(new Date(now.getTime() + ahead * DAY_MS))));
});

/** Every request_YYYYMMDD table, attached or left detached by an interrupted drop. */
export const requestPartitions = Effect.fn("requestPartitions")(function* () {
  const { drizzle } = yield* Database;
  const rows = yield* drizzle.execute<{ relname: string }>(sql`
    select c.relname from pg_class c join pg_namespace n on n.oid = c.relnamespace
    where n.nspname = 'analytics' and c.relkind = 'r' and c.relname ~ '^request_[0-9]{8}$'
    order by c.relname`, "objects");
  return rows.map((row) => `${row.relname.slice(8, 12)}-${row.relname.slice(12, 14)}-${row.relname.slice(14, 16)}`);
});

/** The partition days wholly before every site's retention window. */
export const expiredPartitions = Effect.fn("expiredPartitions")(function* (now: Date) {
  const cutoff = retentionCutoff(now.getTime());
  return (yield* requestPartitions()).filter((day) => Date.parse(`${day}T00:00:00Z`) + DAY_MS <= cutoff.getTime());
});

/** Returns false when a lock wait timed out; the partition is left for the next run. */
export const dropPartition = Effect.fn("dropPartition")(function* (day: string) {
  const { database } = yield* AppConfig;
  const name = `analytics.${partitionName(day)}`;
  return yield* Effect.acquireUseRelease(
    Effect.tryPromise(async () => {
      const client = new Client({ connectionString: database.url.href, lock_timeout: DDL_LOCK_TIMEOUT_MS });
      await client.connect();
      return client;
    }),
    (client) => Effect.tryPromise(async () => {
      try {
        const { rows } = await client.query<{ pending: boolean }>(
          "select inhdetachpending as pending from pg_inherits where inhrelid = to_regclass($1)", [name]);
        const attached = rows[0];
        if (attached !== undefined) {
          await client.query(`alter table analytics.request detach partition ${name} ${attached.pending ? "finalize" : "concurrently"}`);
        }
        await client.query(`drop table if exists ${name}`);
        return true;
      } catch (error) {
        if (error instanceof DatabaseError && error.code === LOCK_NOT_AVAILABLE) return false;
        throw error;
      }
    }),
    (client) => Effect.promise(() => client.end()),
  );
});
