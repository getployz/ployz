import { type SQL, sql } from "drizzle-orm";
import type { EffectDrizzleQueryError } from "drizzle-orm/effect-core";
import { Data, Effect, Exit } from "effect";
import { Database, type DatabaseService } from "#/server/database.server";
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
const STALE = Symbol("stale");

type SiteRow = {
  rolled_up_to: string | null;
  dirty: string[];
  floor: string;
  below: string;
  pending: string[];
  days: Record<string, number>;
  generation: string;
};

/** Inclusive local-day span; published-day metadata outside it stays unbound. */
export type DaySpan = { readonly first: string; readonly last: string };

const SITE_COLUMNS = sql`
  rolled_up_to::text as rolled_up_to, dirty::text[] as dirty, floor::text as floor, below::text as below,
  pending::text[] as pending, days, generation::text as generation`;

const EMPTY_SITE: SiteRow = { rolled_up_to: null, dirty: [], floor: "0", below: "0", pending: [], days: {}, generation: "0" };

export type Bind = (type: string, value: Params[string]) => string;

export type Visibility = { readonly visible: string; readonly publishedDay: string };

const visibility = (site: SiteRow, bind: Bind, within?: DaySpan): Visibility => {
  const days = Object.entries(site.days).filter(([day]) => within === undefined || (day >= within.first && day <= within.last));
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
  readonly visibility: (bind: Bind, within?: DaySpan) => Visibility;
};

const snapshotOf = (site: SiteRow): Snapshot => ({
  rolledUpTo: site.rolled_up_to,
  dirty: site.dirty,
  stamp: new SiteStamp(site),
  visibility: (bind, within) => visibility(site, bind, within),
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

/** Rows reach the caller only if `current` still holds once their select has resolved, so no fiber inside the callback can answer from a collected attempt. */
const sealed = <A, E, R>(clickhouse: ClickHouseService, site: SiteRow, current: Effect.Effect<void, ClickHouseFailure>, body: (reader: Reader) => Effect.Effect<A, E, R>) => {
  let open = true;
  const stillOpen = Effect.suspend(() => open ? Effect.void : Effect.die("A Reader answers only inside the read or locked callback that made it."));
  const reader: Reader = {
    snapshot: snapshotOf(site),
    select: <A>(query: string, params?: Params) => Effect.gen(function* () {
      yield* stillOpen;
      const rows = yield* clickhouse.unchecked.select<A>(query, params);
      yield* stillOpen;
      yield* current;
      return rows;
    }),
  };
  return body(reader).pipe(Effect.ensuring(Effect.sync(() => { open = false; })));
};

const isStale = <E>(error: E): error is E & ClickHouseFailure => error instanceof ClickHouseFailure && error.cause === STALE;

export const read = Effect.fn("WebAnalytics.read")(function* <A, E, R>(clickhouse: ClickHouseService, environmentId: string, body: (reader: Reader) => Effect.Effect<A, E, R>) {
  const database = yield* Database;
  if (database.inTransaction) return yield* Effect.die("read checks the site's generation across its own statements; a transaction would freeze it, so read must not run inside one.");
  const raced = new ClickHouseFailure({ message: `Garbage collection raced a read of site ${environmentId}.`, cause: STALE });
  for (let tries = 0; tries < READ_TRIES; tries++) {
    const before = (yield* siteRow(environmentId, sql``)) ?? EMPTY_SITE;
    const current = siteRow(environmentId, sql``).pipe(
      Effect.mapError((cause) => new ClickHouseFailure({ message: `Postgres could not confirm the read of site ${environmentId}.`, cause })),
      Effect.flatMap((now) => garbageCollectedBetween(before, now ?? EMPTY_SITE) ? Effect.fail(raced) : Effect.void),
      Effect.provideService(Database, database),
    );
    const outcome = yield* sealed(clickhouse, before, current, body).pipe(
      Effect.map((result) => ({ result })),
      Effect.catchIf(isStale, () => Effect.succeed(undefined)),
    );
    if (outcome === undefined) continue;
    const after = (yield* siteRow(environmentId, sql``)) ?? EMPTY_SITE;
    if (!garbageCollectedBetween(before, after)) return outcome.result;
  }
  return yield* new ClickHouseFailure({ message: `Garbage collection raced ${READ_TRIES} reads of site ${environmentId}.`, cause: null });
});

export const locked = Effect.fn("WebAnalytics.lockSite")(function* <A, E, R>(clickhouse: ClickHouseService, environmentId: string, body: (reader: Reader) => Effect.Effect<A, E, R>) {
  const database = yield* Database;
  if (!database.inTransaction) return yield* Effect.die("locked takes the site lock inside the transaction that publishes.");
  const row = yield* siteRow(environmentId, sql`for update`);
  if (row === undefined) return null;
  return yield* sealed(clickhouse, row, Effect.void, body);
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
  | { readonly kind: "day"; readonly day: string; readonly close: boolean; readonly keepDays: number };

export type WriteFailure = ClickHouseFailure | EffectDrizzleQueryError;

export type Writer = {
  readonly attempt: Attempt;
  readonly rows: (table: Table, rows: ReadonlyArray<object>) => Effect.Effect<void, WriteFailure>;
  readonly select: (statements: ReadonlyArray<InsertSelect>) => Effect.Effect<void, WriteFailure>;
  readonly publish: (publication: Publication) => Effect.Effect<void, PublishRefused | EffectDrizzleQueryError, Database>;
};

const STAMPED = new RegExp(`^\\s*INSERT INTO (${TABLES.join("|")}) \\([^)]*\\battempt\\b[^)]*\\)[\\s\\S]*\\{attempt:UInt64\\}`);

const publicationSet = (attempt: Attempt, publication: Publication): SQL => {
  switch (publication.kind) {
    case "append":
      return sql`
        pending = array_remove(pending, ${attempt}::bigint), below = greatest(below, ${attempt}::bigint + 1),
        dirty = array(select distinct d from unnest(dirty || ${sql.param(publication.dirty)}::date[]) d order by d)`;
    case "refloor":
      return sql`
        pending = array(select p from unnest(pending) p where p > ${attempt}::bigint order by p), recovery = null,
        floor = ${attempt}::bigint, below = ${attempt}::bigint + 1, days = '{}'::jsonb,
        rolled_up_to = null, dirty = '{}', reads = false, backfilled_at = now()`;
    case "day":
      return sql`
        pending = array_remove(pending, ${attempt}::bigint), ${publication.close ? sql`rolled_up_to = ${publication.day}::date,` : sql``}
        dirty = array_remove(dirty, ${publication.day}::date),
        days = (select coalesce(jsonb_object_agg(key, value), '{}'::jsonb)
          from (select days || jsonb_build_object(${publication.day}::text, ${attempt}::bigint) as published) p, jsonb_each(p.published)
          where key::date > (select max(k::date) from jsonb_object_keys(p.published) k) - ${publication.keepDays}::int)`;
  }
};

/** A refloor publishes only the current recovery occupant; every other kind publishes a pending append. */
const publishable = (attempt: Attempt, publication: Publication): SQL =>
  publication.kind === "refloor" ? sql`${attempt}::bigint = recovery` : sql`${attempt}::bigint = any(pending)`;

const writer = (clickhouse: ClickHouseService, pool: DatabaseService, environmentId: string, attempt: Attempt): Writer => {
  const state = { inflight: 0, poisoned: false, spent: false };
  const behindFence = Effect.map(
    pool.drizzle.execute<{ fence: string }>(sql`select fence::text as fence from analytics.clickhouse_site where environment_id = ${environmentId}::text`, "objects"),
    ([site]) => site === undefined || attempt < Number(site.fence),
  );
  const guarded = (write: Effect.Effect<void, ClickHouseFailure>) => Effect.suspend(() => {
    if (state.spent) return Effect.die(`Attempt ${attempt} already published; mint a new one before writing more rows.`);
    state.inflight++;
    return behindFence.pipe(
      Effect.flatMap((behind) => behind
        ? new ClickHouseFailure({ message: `Attempt ${attempt} of site ${environmentId} is behind the garbage collection fence; mint a new one.`, cause: null })
        : write),
      Effect.onExit((exit) => Effect.sync(() => {
        state.inflight--;
        if (!Exit.isSuccess(exit)) state.poisoned = true;
      })),
    );
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
        set ${publicationSet(attempt, publication)}, updated_at = now()
        where environment_id = ${environmentId}::text and ${publishable(attempt, publication)} and ${attempt}::bigint >= fence
        returning environment_id`, "objects");
      if (rows.length === 0) return yield* new PublishRefused({ environmentId, attempt });
    }),
  };
};

const ensureSites = (environmentIds: ReadonlyArray<string>) => Effect.gen(function* () {
  const { drizzle } = yield* Database;
  yield* drizzle.execute(sql`
    insert into analytics.clickhouse_site (environment_id)
    select environment_id from analytics.site where environment_id = any(${sql.param(environmentIds)}::text[])
    on conflict do nothing`);
});

/**
 * Mints one append per site with room in pending and no recovery occupant.
 * While a recovery is minted no append can be born after it, so no append publication can ever raise `below` past an unpublished recovery.
 */
export const mint = Effect.fn("WebAnalytics.mintAttempt")(function* (clickhouse: ClickHouseService, environmentIds: ReadonlyArray<string>) {
  const database = yield* outsideTransaction("mint");
  if (environmentIds.length === 0) return new Map<string, Writer>();
  return yield* database.transaction(Effect.gen(function* () {
    const { drizzle } = yield* Database;
    yield* ensureSites(environmentIds);
    const rows = yield* drizzle.execute<{ environment_id: string; attempt: string }>(sql`
      update analytics.clickhouse_site
      set pending = pending || nextval('analytics.clickhouse_attempt'), updated_at = now()
      where cardinality(pending) < ${PENDING_CAP} and recovery is null and environment_id in (
        select environment_id from analytics.clickhouse_site
        where environment_id = any(${sql.param(environmentIds)}::text[]) order by environment_id for update skip locked)
      returning environment_id, pending[cardinality(pending)]::text as attempt`, "objects");
    // SAFETY: each row's `attempt` is the nextval this statement just appended to `pending`, one of the two places an Attempt is born.
    return new Map(rows.map((row) => [row.environment_id, writer(clickhouse, database, row.environment_id, Number(row.attempt) as Attempt)]));
  }));
});

/**
 * Mints the recovery attempt whose refloor rebuilds the site, replacing any occupant: recovery never runs out of room.
 * Demotes reads in the same statement, so a site with an occupant always schedules another recovery until one publishes.
 * A replaced occupant can never publish (publish requires `attempt = recovery`) and stays below the next published floor.
 */
export const mintRecovery = Effect.fn("WebAnalytics.mintRecovery")(function* (clickhouse: ClickHouseService, environmentId: string) {
  const database = yield* outsideTransaction("mintRecovery");
  return yield* database.transaction(Effect.gen(function* () {
    const { drizzle } = yield* Database;
    yield* ensureSites([environmentId]);
    const [row] = yield* drizzle.execute<{ attempt: string }>(sql`
      update analytics.clickhouse_site
      set recovery = nextval('analytics.clickhouse_attempt'), reads = false, backfilled_at = null, updated_at = now()
      where environment_id = ${environmentId}::text
      returning recovery::text as attempt`, "objects");
    if (row === undefined) return yield* new SiteUnavailable({ environmentId });
    // SAFETY: `attempt` is the nextval this statement just stored in `recovery`, the other place an Attempt is born.
    return writer(clickhouse, database, environmentId, Number(row.attempt) as Attempt);
  }));
});

/** A site that cannot mint an append: pending is full or a recovery occupant is waiting for its refloor. Only a published refloor clears it. */
export const PARKED = sql`(cardinality(pending) >= ${PENDING_CAP} or recovery is not null)`;

export const parked = Effect.fn("WebAnalytics.parked")(function* (environmentId: string) {
  const { drizzle } = yield* Database;
  const [row] = yield* drizzle.execute<{ parked: boolean }>(sql`
    select ${PARKED} as parked from analytics.clickhouse_site where environment_id = ${environmentId}::text`, "objects");
  return row?.parked ?? false;
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
    const fenced = yield* database.transaction(Effect.gen(function* () {
      const { drizzle } = yield* Database;
      const [held] = yield* drizzle.execute<{ environment_id: string }>(sql`
        select environment_id from analytics.clickhouse_site where environment_id = ${environmentId}::text for update skip locked`, "objects");
      if (held === undefined) return undefined;
      const [row] = yield* drizzle.execute<SiteRow & { fence: string }>(sql`
        update analytics.clickhouse_site
        set fence = greatest(fence, ${pin}::bigint), generation = generation + 1, updated_at = now()
        where environment_id = ${environmentId}::text
        returning ${SITE_COLUMNS}, fence::text as fence`, "objects");
      return row;
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
