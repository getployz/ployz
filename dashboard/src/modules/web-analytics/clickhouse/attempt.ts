import { type SQL, sql } from "drizzle-orm";
import type { EffectDrizzleQueryError } from "drizzle-orm/effect-core";
import { Data, Effect, Exit } from "effect";
import { Database } from "#/server/database.server";
import { ClickHouseFailure, type ClickHouseService, type Params } from "./client";

declare const attemptBrand: unique symbol;

/** An identity minted by the Postgres sequence. Rows carry it; readers see it only once Postgres publishes it. */
export type Attempt = number & { readonly [attemptBrand]: true };

export class PublishRefused extends Data.TaggedError("PublishRefused")<{ readonly environmentId: string; readonly attempt: number }> {}
export class SiteUnavailable extends Data.TaggedError("SiteUnavailable")<{ readonly environmentId: string }> {}

export const TABLES = ["pageview", "daily", "session_hourly"] as const;
export type Table = (typeof TABLES)[number];

export const PENDING_CAP = 32;

const READ_TRIES = 4;

type SiteRow = {
  rolled_up_to: string | null;
  dirty: string[];
  floor: string;
  below: string;
  pending: string[];
  days: Record<string, number>;
  generation: string;
};

const SITE_COLUMNS = sql`
  rolled_up_to::text as rolled_up_to, dirty::text[] as dirty, floor::text as floor, below::text as below,
  pending::text[] as pending, days, generation::text as generation`;

const EMPTY_SITE: SiteRow = { rolled_up_to: null, dirty: [], floor: "0", below: "0", pending: [], days: {}, generation: "0" };

export type Bind = (type: string, value: Params[string]) => string;

export type Visibility = { readonly visible: string; readonly publishedDay: string };

const visibility = (site: SiteRow, bind: Bind): Visibility => {
  const days = Object.entries(site.days);
  return {
    visible: `(attempt >= ${bind("UInt64", Number(site.floor))} AND attempt < ${bind("UInt64", Number(site.below))} AND NOT has(${bind("Array(UInt64)", site.pending.map(Number))}, attempt))`,
    publishedDay: `attempt = arrayElement(${bind("Array(UInt64)", days.map(([, attempt]) => attempt))}, indexOf(${bind("Array(Date)", days.map(([day]) => day))}, day))`,
  };
};

class SiteStamp {
  readonly #site: SiteRow;
  constructor(site: SiteRow) {
    this.#site = site;
  }
  unchanged(): SQL {
    const site = this.#site;
    return sql`
      rolled_up_to is not distinct from ${site.rolled_up_to}::date and dirty = ${sql.param(site.dirty)}::date[]
      and floor = ${site.floor}::bigint and below = ${site.below}::bigint and pending = ${sql.param(site.pending)}::bigint[]
      and days = ${JSON.stringify(site.days)}::jsonb and generation = ${site.generation}::bigint`;
  }
}
export type Stamp = SiteStamp;

export type Snapshot = {
  readonly rolledUpTo: string | null;
  readonly dirty: ReadonlyArray<string>;
  readonly stamp: Stamp;
  readonly visibility: (bind: Bind) => Visibility;
};

const snapshotOf = (site: SiteRow): Snapshot => ({
  rolledUpTo: site.rolled_up_to,
  dirty: site.dirty,
  stamp: new SiteStamp(site),
  visibility: (bind) => visibility(site, bind),
});

export type Reader = {
  readonly snapshot: Snapshot;
  readonly select: <A>(query: string, params?: Params) => Effect.Effect<ReadonlyArray<A>, ClickHouseFailure>;
};

const siteRow = Effect.fn("WebAnalytics.siteRow")(function* (environmentId: string, lock: SQL) {
  const { drizzle } = yield* Database;
  const [row] = yield* drizzle.execute<SiteRow>(sql`
    select ${SITE_COLUMNS} from analytics.clickhouse_site where environment_id = ${environmentId}::text ${lock}`, "objects");
  return row;
});

const garbageCollectedBetween = (before: SiteRow, after: SiteRow) => after.generation !== before.generation;

export const read = Effect.fn("WebAnalytics.read")(function* <A, E, R>(clickhouse: ClickHouseService, environmentId: string, body: (reader: Reader) => Effect.Effect<A, E, R>) {
  for (let tries = 0; tries < READ_TRIES; tries++) {
    const before = (yield* siteRow(environmentId, sql``)) ?? EMPTY_SITE;
    const result = yield* body({ snapshot: snapshotOf(before), select: clickhouse.unchecked.select });
    const after = (yield* siteRow(environmentId, sql``)) ?? EMPTY_SITE;
    if (!garbageCollectedBetween(before, after)) return result;
  }
  return yield* new ClickHouseFailure({ message: `Garbage collection raced ${READ_TRIES} reads of site ${environmentId}.`, cause: null });
});

export const locked = Effect.fn("WebAnalytics.lockSite")(function* (clickhouse: ClickHouseService, environmentId: string) {
  const database = yield* Database;
  if (!database.inTransaction) return yield* Effect.die("locked takes the site lock inside the transaction that publishes.");
  const row = yield* siteRow(environmentId, sql`for update`);
  if (row === undefined) return null;
  return { snapshot: snapshotOf(row), select: clickhouse.unchecked.select } satisfies Reader;
});

const outsideTransaction = (what: string) => Effect.gen(function* () {
  const database = yield* Database;
  if (database.inTransaction) return yield* Effect.die(`${what} commits on its own; it must not run inside a caller's transaction.`);
  return database;
});

export type InsertSelect = { readonly query: string; readonly params: Params };

export type Publication =
  | { readonly kind: "append"; readonly dirty: ReadonlyArray<string> }
  | { readonly kind: "refloor" }
  | { readonly kind: "day"; readonly day: string; readonly close: boolean };

export type Writer = {
  readonly attempt: Attempt;
  readonly rows: (table: Table, rows: ReadonlyArray<object>) => Effect.Effect<void, ClickHouseFailure>;
  readonly select: (statements: ReadonlyArray<InsertSelect>) => Effect.Effect<void, ClickHouseFailure>;
  readonly publish: (publication: Publication) => Effect.Effect<void, PublishRefused | EffectDrizzleQueryError, Database>;
};

const STAMPED = new RegExp(`^\\s*INSERT INTO (${TABLES.join("|")}) \\([^)]*\\battempt\\b[^)]*\\)[\\s\\S]*\\{attempt:UInt64\\}`);

const publicationSet = (attempt: Attempt, publication: Publication): SQL => {
  switch (publication.kind) {
    case "append":
      return sql`
        below = greatest(below, ${attempt}::bigint + 1),
        dirty = array(select distinct d from unnest(dirty || ${sql.param(publication.dirty)}::date[]) d order by d)`;
    case "refloor":
      return sql`
        floor = ${attempt}::bigint, below = ${attempt}::bigint + 1, days = '{}'::jsonb,
        rolled_up_to = null, dirty = '{}', reads = false, backfilled_at = now()`;
    case "day":
      return sql`
        ${publication.close ? sql`rolled_up_to = ${publication.day}::date,` : sql``}
        dirty = array_remove(dirty, ${publication.day}::date), days = days || jsonb_build_object(${publication.day}::text, ${attempt}::bigint)`;
  }
};

const writer = (clickhouse: ClickHouseService, environmentId: string, attempt: Attempt): Writer => {
  const state = { inflight: 0, poisoned: false, spent: false };
  const guarded = (write: Effect.Effect<void, ClickHouseFailure>) => Effect.suspend(() => {
    if (state.spent) return Effect.die(`Attempt ${attempt} already published; mint a new one before writing more rows.`);
    state.inflight++;
    return write.pipe(Effect.onExit((exit) => Effect.sync(() => {
      state.inflight--;
      if (!Exit.isSuccess(exit)) state.poisoned = true;
    })));
  });
  return {
    attempt,
    rows: (table, rows) => guarded(clickhouse.unchecked.insert(table, rows.map((row) => ({ ...row, attempt })))),
    select: (statements) => guarded(Effect.forEach(statements, (statement) =>
      STAMPED.test(statement.query)
        ? clickhouse.unchecked.command(statement.query, { ...statement.params, attempt })
        : Effect.die("Writer.select takes INSERT INTO statements that write {attempt:UInt64} into the attempt column."), { discard: true })),
    publish: Effect.fn("WebAnalytics.publish")(function* (publication: Publication) {
      const database = yield* Database;
      if (!database.inTransaction) return yield* Effect.die("publish runs inside the transaction that commits the publication.");
      if (state.spent) return yield* Effect.die(`Attempt ${attempt} already published; it cannot publish twice.`);
      if (state.inflight > 0) return yield* Effect.die(`Attempt ${attempt} published while a write under it was still in flight.`);
      state.spent = true;
      if (state.poisoned) return yield* new PublishRefused({ environmentId, attempt });
      const rows = yield* database.drizzle.execute<{ environment_id: string }>(sql`
        update analytics.clickhouse_site
        set pending = array_remove(pending, ${attempt}::bigint), ${publicationSet(attempt, publication)}, updated_at = now()
        where environment_id = ${environmentId}::text and ${attempt}::bigint = any(pending) and ${attempt}::bigint >= fence
        returning environment_id`, "objects");
      if (rows.length === 0) return yield* new PublishRefused({ environmentId, attempt });
    }),
  };
};

export const mint = Effect.fn("WebAnalytics.mintAttempt")(function* (clickhouse: ClickHouseService, environmentIds: ReadonlyArray<string>) {
  const database = yield* outsideTransaction("mint");
  if (environmentIds.length === 0) return new Map<string, Writer>();
  return yield* database.transaction(Effect.gen(function* () {
    const { drizzle } = yield* Database;
    yield* drizzle.execute(sql`
      insert into analytics.clickhouse_site (environment_id)
      select environment_id from analytics.site where environment_id = any(${sql.param(environmentIds)}::text[])
      on conflict do nothing`);
    const full = sql`cardinality(pending) >= ${PENDING_CAP}`;
    const past = sql`greatest(below, pending[cardinality(pending)] + 1)`;
    const rows = yield* drizzle.execute<{ environment_id: string; attempt: string }>(sql`
      update analytics.clickhouse_site
      set pending = case when ${full} then array[nextval('analytics.clickhouse_attempt')] else pending || nextval('analytics.clickhouse_attempt') end,
        floor = case when ${full} then ${past} else floor end,
        below = case when ${full} then ${past} else below end,
        reads = case when ${full} then false else reads end,
        backfilled_at = case when ${full} then null else backfilled_at end,
        updated_at = now()
      where environment_id in (
        select environment_id from analytics.clickhouse_site
        where environment_id = any(${sql.param(environmentIds)}::text[]) order by environment_id for update skip locked)
      returning environment_id, pending[cardinality(pending)]::text as attempt`, "objects");
    // SAFETY: each row's `attempt` is the nextval this statement just appended to `pending`, the only place an Attempt is born.
    return new Map(rows.map((row) => [row.environment_id, writer(clickhouse, row.environment_id, Number(row.attempt) as Attempt)]));
  }));
});

/** A sequence value taken now. Every attempt minted before it is smaller. */
export const pin = Effect.fn("WebAnalytics.pinAttempts")(function* () {
  const { drizzle } = yield* Database;
  const [row] = yield* drizzle.execute<{ n: string }>(sql`select nextval('analytics.clickhouse_attempt')::text as n`, "objects");
  if (row === undefined) return yield* Effect.die("nextval returned no row.");
  return Number(row.n);
});

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
      return yield* drizzle.execute<SiteRow & { fence: string }>(sql`
        update analytics.clickhouse_site
        set fence = greatest(fence, ${pin}::bigint),
          generation = generation + 1,
          pending = array(select p from unnest(pending) p where p >= floor order by p),
          updated_at = now()
        where environment_id = ${environmentId}::text
        returning ${SITE_COLUMNS}, fence::text as fence`, "objects");
    }));
    if (fenced === undefined) continue;
    const bound: Array<[string, Params[string]]> = [];
    const kept = visibility(fenced, (type, value) => {
      bound.push([`p${bound.length}`, value]);
      return `{p${bound.length - 1}:${type}}`;
    });
    const params = { env: environmentId, fence: Number(fenced.fence), ...Object.fromEntries(bound) };
    for (const [table, published] of [["pageview", kept.visible], ["daily", kept.publishedDay], ["session_hourly", kept.publishedDay]] as const) {
      const where = `environment_id = {env:String} AND attempt < {fence:UInt64} AND NOT ${published}`;
      const [row] = yield* clickhouse.unchecked.select<{ n: number }>(`SELECT count() AS n FROM ${table} WHERE ${where}`, params);
      const n = Number(row?.n ?? 0);
      if (n === 0) continue;
      yield* clickhouse.unchecked.command(`DELETE FROM ${table} WHERE ${where}`, params);
      collected += n;
    }
  }
  return collected;
});

export const deleteSite = (clickhouse: ClickHouseService, environmentId: string) =>
  Effect.forEach(TABLES, (table) => clickhouse.unchecked.command(`DELETE FROM ${table} WHERE environment_id = {environmentId:String}`, { environmentId }), { discard: true });
