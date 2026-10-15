import { sql } from "drizzle-orm";
import { Effect } from "effect";
import { Client, DatabaseError } from "pg";
import { AppConfig } from "#/server/config.server";
import { Database, sqlErrorFrom } from "#/server/database.server";

export const monthOf = (date: Date) => date.toISOString().slice(0, 7);

export const nextMonth = (month: string) => {
  return monthOf(new Date(Date.UTC(Number(month.slice(0, 4)), Number(month.slice(5, 7)), 1)));
};

export const partitionName = (month: string) => `pageview_${month.replace("-", "")}`;

const DUPLICATE_TABLE = "42P07";
const UNIQUE_VIOLATION = "23505";
const LOCK_NOT_AVAILABLE = "55P03";
const DDL_LOCK_TIMEOUT_MS = 5000;

const pgCode = (cause: unknown) => {
  const driverError = sqlErrorFrom(cause)?.reason.cause;
  return driverError instanceof DatabaseError ? driverError.code : undefined;
};

const ensured = new Set<string>();

export const ensurePartitions = Effect.fn("ensurePartitions")(function* (months: ReadonlyArray<string>) {
  const missing = [...new Set(months)].filter((month) => !ensured.has(month));
  if (missing.length === 0) return;
  const database = yield* Database;
  for (const month of missing) {
    yield* database.transaction(Effect.gen(function* () {
      const { drizzle } = yield* Database;
      yield* drizzle.execute(sql.raw(`set local lock_timeout = '${DDL_LOCK_TIMEOUT_MS}ms'`));
      yield* drizzle.execute(sql.raw(
        `create table if not exists analytics.${partitionName(month)} partition of analytics.pageview `
        + `for values from ('${month}-01 00:00+00') to ('${nextMonth(month)}-01 00:00+00')`,
      ));
    })).pipe(Effect.catchIf(
      (error) => pgCode(error) === DUPLICATE_TABLE || pgCode(error) === UNIQUE_VIOLATION,
      () => Effect.void,
    ));
    ensured.add(month);
  }
});

export const forgetPartitions = () => ensured.clear();

/** Every pageview_YYYYMM table, attached or left detached by an interrupted drop. */
export const pageviewPartitions = Effect.fn("pageviewPartitions")(function* () {
  const { drizzle } = yield* Database;
  const rows = yield* drizzle.execute<{ relname: string }>(sql`
    select c.relname from pg_class c join pg_namespace n on n.oid = c.relnamespace
    where n.nspname = 'analytics' and c.relkind = 'r' and c.relname ~ '^pageview_[0-9]{6}$'
    order by c.relname`, "objects");
  return rows.map((row) => `${row.relname.slice(9, 13)}-${row.relname.slice(13, 15)}`);
});

/** Returns false when a lock wait timed out; the partition is left for the next run. */
export const dropPartition = Effect.fn("dropPartition")(function* (month: string) {
  const { database } = yield* AppConfig;
  const name = `analytics.${partitionName(month)}`;
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
          await client.query(`alter table analytics.pageview detach partition ${name} ${attached.pending ? "finalize" : "concurrently"}`);
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
