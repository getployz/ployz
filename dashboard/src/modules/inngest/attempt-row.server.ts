import "@tanstack/react-start/server-only";
import { and, desc, eq, getTableName, inArray, type InferInsertModel, type InferSelectModel, isNull, lt, or, type SQL, sql } from "drizzle-orm";
import type { PgColumn, PgTable } from "drizzle-orm/pg-core";
import { Effect, Schedule } from "effect";
import { Database } from "#/server/database.server";
import { NotFound } from "#/server/public-error";
import { checkedNow } from "#/db/tables";

/**
 * A row-backed attempt (a Drain, a web traffic change, a Namespace clean): one Cloud-owned row, written `pending` by the
 * request, bound to the Inngest run that owns it, claimed `running` right before the Engine is asked, and ended with
 * the Engine's answer. Only the owning run may claim or end it. This is the SQL every such table shares.
 */
export type AttemptTable = PgTable & {
  readonly id: PgColumn;
  readonly organizationId: PgColumn;
  readonly state: PgColumn;
  readonly inngestRunId: PgColumn;
  readonly requestedAt: PgColumn;
  readonly startedAt: PgColumn;
  readonly endedAt: PgColumn;
};

/** Columns of `T` to write; an expression (`case when …`) may stand for a value. */
type Values<T extends AttemptTable> = { [K in keyof InferInsertModel<T>]?: InferInsertModel<T>[K] | SQL };
type StateOf<T extends AttemptTable> = InferSelectModel<T>["state"];

/** The longest words a row keeps of why it ended; the table's checks refuse more. */
export const MESSAGE_LIMIT = 1024;

export const within = (text: string) => (text.length > MESSAGE_LIMIT ? `${text.slice(0, MESSAGE_LIMIT - 1)}…` : text);

const ACTIVE_STATES = ["pending", "running"] as const;

/** What a step finds once the row is claimed and the Engine may have been asked. */
type Prepared<Ready, State> = { readonly ready: Ready } | { readonly settled: State };

/** `prepare` found what the call needs. */
export const ready = <Ready>(value: Ready) => ({ ready: value }) as const;
/** `prepare` settled the row itself: the run is done, with the row in `state`. */
export const settled = <State>(state: State) => ({ settled: state }) as const;

/**
 * Drains and changes in an Organization take turns: a pending row's time runs from when the row before it
 * ended, and only while no row ahead of it is running or pending. `earlierOnly` counts only rows requested before it.
 * Both times are Postgres's, so the wait is measured on its clock. Spelled out: an update renders its columns
 * unqualified, which inside a subquery would name the alias's.
 */
export function queueWaitedPast(table: AttemptTable, limitMs: number, { earlierOnly }: { readonly earlierOnly: boolean }) {
  const name = sql.raw(getTableName(table));
  return [
    sql`not exists (
        select 1 from ${name} as ahead
        where ahead.organization_id = ${name}.organization_id
          and (ahead.state = 'running' or (ahead.state = 'pending' and ahead.requested_at < ${name}.requested_at))
      )`,
    sql`greatest(${name}.requested_at, (
        select max(before.ended_at) from ${name} as before
        where before.organization_id = ${name}.organization_id${earlierOnly ? sql`
          and before.requested_at < ${name}.requested_at` : sql``}
      )) < ${checkedNow()} - make_interval(secs => ${limitMs / 1000})`,
  ];
}

/** The shared claim, write and close SQL of one attempt table; `notFound` is what a missing row says. */
export function attemptRows<T extends AttemptTable>(concrete: T, notFound: string) {
  // Only the shared columns are read or written here, so the table is used as that much of it; `T` types the values.
  const table: AttemptTable = concrete;
  /** The row is this run's own: only then may it claim or end it. */
  const owned = (id: string, runId: string) => and(eq(table.id, id), eq(table.inngestRunId, runId));
  /** The row is pending and no other run holds it. */
  const unbound = (id: string, runId: string) => and(
    eq(table.id, id),
    eq(table.state, "pending"),
    or(isNull(table.inngestRunId), eq(table.inngestRunId, runId)),
  );
  const active = inArray(table.state, ACTIVE_STATES);

  const stateOf = Effect.fn("AttemptRow.stateOf")(function* (id: string) {
    const { drizzle } = yield* Database;
    const [row] = yield* drizzle.select({ state: table.state }).from(table).where(eq(table.id, id));
    if (row === undefined) return yield* new NotFound({ message: notFound });
    const state: StateOf<T> = row.state;
    return state;
  });

  /**
   * Bind the run to its pending row, so only it may claim and end the row. `scope` is what the event must name beside
   * the row's id (its Organization, its Server). A row that is no longer pending (bound by another run, or already
   * ended) is settled: the run does nothing. A row `scope` doesn't match is not found.
   */
  const bind = Effect.fn("AttemptRow.bind")(function* (id: string, runId: string, scope: readonly SQL[]) {
    const { drizzle } = yield* Database;
    const [bound] = yield* drizzle.update(table).set({ inngestRunId: runId }).where(and(
      eq(table.id, id),
      ...scope,
      eq(table.state, "pending"),
      or(isNull(table.inngestRunId), eq(table.inngestRunId, runId)),
    )).returning({ id: table.id });
    if (bound !== undefined) return { kind: "bound" } as const;
    const [row] = yield* drizzle.select({ state: table.state }).from(table).where(and(eq(table.id, id), ...scope));
    if (row === undefined) return yield* new NotFound({ message: notFound });
    const state: StateOf<T> = row.state;
    return { kind: "settled", state } as const;
  });

  /** Claim the pending row `running`. False when it isn't pending or isn't this run's. */
  const claim = Effect.fn("AttemptRow.claim")(function* (id: string, runId: string) {
    const { drizzle } = yield* Database;
    const [claimed] = yield* drizzle.update(table).set({ state: "running", startedAt: sql`now()` })
      .where(and(owned(id, runId), eq(table.state, "pending"))).returning({ id: table.id });
    return claimed !== undefined;
  });

  /** End the row this run owns while it is in one of `from`; the state it ended in, or null when it wasn't. */
  const end = Effect.fn("AttemptRow.end")(function* (id: string, runId: string, values: Values<T>, from: readonly StateOf<T>[]) {
    const { drizzle } = yield* Database;
    const [ended] = yield* drizzle.update(table).set(values)
      .where(and(owned(id, runId), inArray(table.state, from))).returning({ state: table.state });
    if (ended === undefined) return null;
    const state: StateOf<T> = ended.state;
    return state;
  });

  /**
   * Ask the Engine once. The row is claimed (`running`) right before the call and ended with its answer right after,
   * in one step: a retry that finds the row running knows the Engine may have been asked, ends it with `lost` and never
   * asks again; one that finds it ended returns what's there. `prepare` reads whatever the call needs while the row is
   * still pending, so a failure there is retried rather than ending a row the Engine was never asked about; it may
   * settle the row itself instead. `call` answers with the values that end the row; the write is retried here rather
   * than by asking again. An end replaces only a row in `endsFrom`.
   */
  const executeOnce = Effect.fn("AttemptRow.execute")(function* <Ready, PrepareError, PrepareContext, CallError, CallContext>(
    input: {
      readonly id: string;
      readonly runId: string;
      readonly lost: Values<T>;
      readonly endsFrom: readonly StateOf<T>[];
      readonly prepare: Effect.Effect<Prepared<Ready, StateOf<T>>, PrepareError, PrepareContext>;
      readonly call: (ready: Ready) => Effect.Effect<Values<T>, CallError, CallContext>;
    },
  ) {
    const { id, runId } = input;
    const current = yield* stateOf(id);
    if (current === "running") {
      yield* end(id, runId, input.lost, ["running"]);
      return yield* stateOf(id);
    }
    if (current !== "pending") return current;
    const prepared = yield* input.prepare;
    if ("settled" in prepared) return prepared.settled;
    if (!(yield* claim(id, runId))) return yield* stateOf(id);
    const values = yield* input.call(prepared.ready);
    const written = yield* end(id, runId, values, input.endsFrom).pipe(
      Effect.retry({ times: 4, schedule: Schedule.exponential("500 millis") }),
    );
    return written ?? (yield* stateOf(id));
  });

  /**
   * A run that failed for good or was cancelled must not leave its row active. `ends` names the columns that end it, by
   * whether the row was still pending (the Engine never asked) or running. A run cancelled before it bound its row names
   * the row by `attemptId` from its event. How many rows this call ended.
   */
  const closeRun = Effect.fn("AttemptRow.closeRun")(function* (
    runId: string,
    attemptId: string | undefined,
    ends: (whenPending: <V extends string>(pending: V, running: V) => SQL<V>) => Values<T>,
  ) {
    const { drizzle } = yield* Database;
    const isPending = sql`${table.state} = 'pending'`;
    const rows = yield* drizzle.update(table).set({
      ...ends(<V extends string>(pending: V, running: V) => sql<V>`case when ${isPending} then ${pending} else ${running} end`),
      endedAt: sql`now()`,
    })
      .where(or(
        and(eq(table.inngestRunId, runId), active),
        attemptId === undefined ? undefined : unbound(attemptId, runId),
      ))
      .returning({ id: table.id });
    return rows.length;
  });

  /**
   * The hourly sweep: a row running past `running.limitMs` lost its run; a pending one `pending.when` matches never
   * starts. How many rows each ended.
   */
  const closeStale = Effect.fn("AttemptRow.closeStale")(function* (input: {
    readonly running: { readonly limitMs: number; readonly end: Values<T> };
    readonly pending: { readonly end: Values<T>; readonly when: SQL | readonly SQL[] };
  }) {
    const { drizzle } = yield* Database;
    const running = yield* drizzle.update(table).set(input.running.end)
      .where(and(eq(table.state, "running"), lt(table.startedAt, sql`${checkedNow()} - make_interval(secs => ${input.running.limitMs / 1000})`)))
      .returning({ id: table.id });
    const when = input.pending.when;
    const pending = yield* drizzle.update(table).set(input.pending.end)
      .where(and(eq(table.state, "pending"), ...(Array.isArray(when) ? when : [when])))
      .returning({ id: table.id });
    return { running: running.length, pending: pending.length };
  });

  return { active, owned, stateOf, bind, claim, end, executeOnce, closeRun, closeStale };
}

/** Each Machine's latest attempt in the Organization: `table`'s rows, newest per Machine. */
export const latestPerMachine = Effect.fn("AttemptRow.latestPerMachine")(function* <T extends AttemptTable & { readonly machineId: PgColumn }>(
  concrete: T, organizationId: string,
) {
  const table: AttemptTable & { readonly machineId: PgColumn } = concrete;
  const { drizzle } = yield* Database;
  const rows = yield* drizzle.selectDistinctOn([table.machineId]).from(table)
    .where(eq(table.organizationId, organizationId))
    .orderBy(table.machineId, desc(table.requestedAt), desc(table.id));
  // SAFETY: the select reads every column of `concrete`, whose rows these are.
  return rows as InferSelectModel<T>[];
});
