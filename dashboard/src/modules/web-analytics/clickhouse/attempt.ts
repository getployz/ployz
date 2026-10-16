import { type SQL, sql } from "drizzle-orm";
import { Data, Effect } from "effect";
import { Database } from "#/server/database.server";
import type { ClickHouseService, Params } from "./client";

declare const attemptBrand: unique symbol;
declare const acknowledgedBrand: unique symbol;

/** An identity minted by the Postgres sequence. Rows carry it; readers see it only once Postgres publishes it. */
export type Attempt = number & { readonly [attemptBrand]: true };

/** Proof that ClickHouse acknowledged every row written under `attempt`. Only `insertRows` and `insertSelect` produce one. */
export type Acknowledged = { readonly attempt: Attempt; readonly [acknowledgedBrand]: true };

/** The Postgres-committed set of visible attempts: pageviews in `[floor, below)` minus `pending`, and one attempt per rolled-up day. */
export type Published = {
  readonly floor: number;
  readonly below: number;
  readonly pending: ReadonlyArray<number>;
  readonly days: Readonly<Record<string, number>>;
};

export class PublishRefused extends Data.TaggedError("PublishRefused")<{ readonly environmentId: string; readonly attempt: number }> {}
export class SiteUnavailable extends Data.TaggedError("SiteUnavailable")<{ readonly environmentId: string }> {}

export const TABLES = ["pageview", "daily", "session_hourly"] as const;
export type Table = (typeof TABLES)[number];

type PublishedRow = { floor: string; below: string; pending: string[]; days: Record<string, number> };

export const PUBLISHED_COLUMNS = sql`floor::text as floor, below::text as below, pending::text[] as pending, days`;

export const publishedOf = (row: PublishedRow | undefined): Published => ({
  floor: Number(row?.floor ?? 0),
  below: Number(row?.below ?? 0),
  pending: row?.pending.map(Number) ?? [],
  days: row?.days ?? {},
});

export type Bind = (type: string, value: Params[string]) => string;

/** The two ClickHouse predicates that read `published`. `visible` selects pageview rows, `publishedDay` selects rollup rows by their `day`. */
export const visibility = (published: Published, bind: Bind) => {
  const days = Object.entries(published.days);
  return {
    visible: `(attempt >= ${bind("UInt64", published.floor)} AND attempt < ${bind("UInt64", published.below)} AND NOT has(${bind("Array(UInt64)", published.pending)}, attempt))`,
    publishedDay: `attempt = arrayElement(${bind("Array(UInt64)", days.map(([, attempt]) => attempt))}, indexOf(${bind("Array(Date)", days.map(([day]) => day))}, day))`,
  };
};

const outsideTransaction = (what: string) => Effect.gen(function* () {
  const database = yield* Database;
  if (database.inTransaction) return yield* Effect.die(`${what} commits on its own; it must not run inside a caller's transaction.`);
  return database;
});

/**
 * Mints one fresh attempt per site in its own committed transaction, so the attempt is pending before any row carries it.
 * A site whose row another writer holds, or that no longer exists, gets no attempt.
 */
export const mint = Effect.fn("WebAnalytics.mintAttempt")(function* (environmentIds: ReadonlyArray<string>) {
  const database = yield* outsideTransaction("mint");
  if (environmentIds.length === 0) return new Map<string, Attempt>();
  return yield* database.transaction(Effect.gen(function* () {
    const { drizzle } = yield* Database;
    yield* drizzle.execute(sql`
      insert into analytics.clickhouse_site (environment_id)
      select environment_id from analytics.site where environment_id = any(${sql.param(environmentIds)}::text[])
      on conflict do nothing`);
    const rows = yield* drizzle.execute<{ environment_id: string; attempt: string }>(sql`
      update analytics.clickhouse_site
      set pending = pending || nextval('analytics.clickhouse_attempt'), updated_at = now()
      where environment_id in (
        select environment_id from analytics.clickhouse_site
        where environment_id = any(${sql.param(environmentIds)}::text[]) order by environment_id for update skip locked)
      returning environment_id, pending[cardinality(pending)]::text as attempt`, "objects");
    // SAFETY: each row's `attempt` is the nextval this statement just appended to `pending`, the only place an Attempt is born.
    return new Map(rows.map((row) => [row.environment_id, Number(row.attempt) as Attempt]));
  }));
});

// SAFETY: called only after the ClickHouse insert or command for `attempt` returned, so the rows are acknowledged.
const acknowledged = (attempt: Attempt) => ({ attempt }) as Acknowledged;

export const insertRows = (clickhouse: ClickHouseService, table: Table, rows: ReadonlyArray<object>, attempt: Attempt) =>
  clickhouse.unchecked.insert(table, rows.map((row) => ({ ...row, attempt }))).pipe(Effect.as(acknowledged(attempt)));

export type InsertSelect = { readonly query: string; readonly params: Params };

const STAMPED = /^\s*INSERT INTO (pageview|daily|session_hourly) \([^)]*\battempt\b[^)]*\)[\s\S]*\{attempt:UInt64\}/;

/** Runs `INSERT INTO ... SELECT` statements that write `{attempt:UInt64}` into their `attempt` column. */
export const insertSelect = Effect.fn("WebAnalytics.insertSelect")(function* (clickhouse: ClickHouseService, attempt: Attempt, statements: ReadonlyArray<InsertSelect>) {
  for (const statement of statements) {
    if (!STAMPED.test(statement.query)) return yield* Effect.die("insertSelect takes INSERT INTO statements that write {attempt:UInt64} into the attempt column.");
    yield* clickhouse.unchecked.command(statement.query, { ...statement.params, attempt });
  }
  return acknowledged(attempt);
});

/**
 * Makes an acknowledged attempt visible by applying `set` to the site's published state, inside the caller's transaction.
 * Refuses an attempt that is no longer pending or that garbage collection fenced.
 */
export const publish = Effect.fn("WebAnalytics.publish")(function* (proof: Acknowledged, environmentId: string, set: SQL) {
  const database = yield* Database;
  if (!database.inTransaction) return yield* Effect.die("publish runs inside the transaction that commits the publication.");
  const attempt = proof.attempt;
  const rows = yield* database.drizzle.execute<{ environment_id: string }>(sql`
    update analytics.clickhouse_site
    set pending = array_remove(pending, ${attempt}::bigint), ${set}, updated_at = now()
    where environment_id = ${environmentId}::text and ${attempt}::bigint = any(pending) and ${attempt}::bigint >= fence
    returning environment_id`, "objects");
  if (rows.length === 0) return yield* new PublishRefused({ environmentId, attempt });
});

/** A sequence value taken now. Every attempt minted before it is smaller. */
export const pin = Effect.gen(function* () {
  const { drizzle } = yield* Database;
  const [row] = yield* drizzle.execute<{ n: string }>(sql`select nextval('analytics.clickhouse_attempt')::text as n`, "objects");
  if (row === undefined) return yield* Effect.die("nextval returned no row.");
  return Number(row.n);
});

/**
 * Per site: commits `fence = max(fence, pin)` so no attempt below it can publish any more, then deletes the rows below the fence
 * that the state committed with the fence does not publish. A site whose row another writer holds is skipped until the next run.
 * Pending attempts stay listed until a backfill moves `floor` past them, so a late insert under one never becomes visible.
 */
export const collectGarbage = Effect.fn("WebAnalytics.collectGarbage")(function* (clickhouse: ClickHouseService, pin: number) {
  const database = yield* outsideTransaction("collectGarbage");
  const sites = yield* database.drizzle.execute<{ environment_id: string }>(sql`
    select environment_id from analytics.clickhouse_site order by environment_id`, "objects");
  let collected = 0;
  for (const { environment_id: environmentId } of sites) {
    const [fenced] = yield* database.transaction(Effect.gen(function* () {
      const { drizzle } = yield* Database;
      const locked = yield* drizzle.execute<{ environment_id: string }>(sql`
        select environment_id from analytics.clickhouse_site where environment_id = ${environmentId}::text for update skip locked`, "objects");
      if (locked.length === 0) return [];
      return yield* drizzle.execute<PublishedRow & { fence: string }>(sql`
        update analytics.clickhouse_site
        set fence = greatest(fence, ${pin}::bigint),
          pending = array(select p from unnest(pending) p where p >= floor order by p),
          updated_at = now()
        where environment_id = ${environmentId}::text
        returning ${PUBLISHED_COLUMNS}, fence::text as fence`, "objects");
    }));
    if (fenced === undefined) continue;
    const bound: Array<[string, Params[string]]> = [];
    const kept = visibility(publishedOf(fenced), (type, value) => {
      bound.push([`p${bound.length}`, value]);
      return `{p${bound.length - 1}:${type}}`;
    });
    const params = { env: environmentId, fence: Number(fenced.fence), ...Object.fromEntries(bound) };
    for (const [table, published] of [["pageview", kept.visible], ["daily", kept.publishedDay], ["session_hourly", kept.publishedDay]] as const) {
      const where = `environment_id = {env:String} AND attempt < {fence:UInt64} AND NOT ${published}`;
      const [row] = yield* clickhouse.query<{ n: number }>(`SELECT count() AS n FROM ${table} WHERE ${where}`, params);
      if (row === undefined || Number(row.n) === 0) continue;
      yield* clickhouse.unchecked.command(`DELETE FROM ${table} WHERE ${where}`, params);
      collected += Number(row.n);
    }
  }
  return collected;
});

export const deleteSite = (clickhouse: ClickHouseService, environmentId: string) =>
  Effect.forEach(TABLES, (table) => clickhouse.unchecked.command(`DELETE FROM ${table} WHERE environment_id = {environmentId:String}`, { environmentId }), { discard: true });
