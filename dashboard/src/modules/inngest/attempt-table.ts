import { sql } from "drizzle-orm";
import { check, type IndexColumn, text, timestamp, uniqueIndex, uuid } from "drizzle-orm/pg-core";
import { sqlStringLiterals } from "#/db/tables";
import { MACHINE_ID_SQL } from "#/lib/schema";
import { user } from "#/modules/identity/tables";
import { organization } from "#/modules/organization/tables";

/**
 * The columns every row-backed attempt carries (see `inngest/attempt-row.server.ts`): the request id the caller minted,
 * who asked, the state it moves through, the run that owns it, and when it was asked, claimed and ended.
 */
export function attemptColumns<State extends string>() {
  return {
    id: uuid("id").primaryKey(),
    organizationId: uuid("organization_id")
      .notNull()
      .references(() => organization.id, { onDelete: "cascade" }),
    /** Who asked; kept when the user is deleted. */
    requestedByUserId: uuid("requested_by_user_id").references(() => user.id, { onDelete: "set null" }),
    state: text("state").default("pending").notNull().$type<State>(),
    /** The run that owns the row, bound while pending; only it may claim or end the row. */
    inngestRunId: text("inngest_run_id"),
    requestedAt: timestamp("requested_at", { mode: "date", withTimezone: true }).defaultNow().notNull(),
    startedAt: timestamp("started_at", { mode: "date", withTimezone: true }),
    endedAt: timestamp("ended_at", { mode: "date", withTimezone: true }),
  };
}

type AttemptKeys = Readonly<Record<"organizationId" | "state" | "inngestRunId", IndexColumn>>;

/**
 * The constraints every attempt table shares, named `<name>_…`: one active row per `key` within an Organization, one
 * row per run, a bounded run id, and a state from `states`.
 */
export function attemptConstraints(
  name: string,
  table: AttemptKeys,
  key: IndexColumn,
  states: readonly string[],
) {
  return [
    uniqueIndex(`${name}_one_active_idx`)
      .on(table.organizationId, key)
      .where(sql`${table.state} in ('pending', 'running')`),
    uniqueIndex(`${name}_run_uidx`)
      .on(table.inngestRunId)
      .where(sql`${table.inngestRunId} is not null`),
    check(`${name}_state_check`, sql`${table.state} in (${sqlStringLiterals(states)})`),
    check(`${name}_run_check`, sql`${table.inngestRunId} is null or length(${table.inngestRunId}) between 1 and 255`),
  ];
}

/** At most one attempt consumes an approval. */
export function approvalIndex(name: string, approvalId: IndexColumn) {
  return uniqueIndex(`${name}_approval_uidx`).on(approvalId).where(sql`${approvalId} is not null`);
}

/** The Machine ID is one the Engine names. */
export function machineIdCheck(name: string, machineId: IndexColumn) {
  return check(`${name}_machine_id_check`, sql`${machineId} ~ '${sql.raw(MACHINE_ID_SQL)}'`);
}

/** Each end code ends in exactly its state. */
export function endCodeCheck(
  name: string,
  table: { readonly state: IndexColumn; readonly endCode: IndexColumn },
  endCodes: Readonly<Record<string, string>>,
) {
  const pairs = sql.join(
    Object.entries(endCodes).map(([code, state]) => sql`(${sqlStringLiterals([code, state])})`),
    sql`,`,
  );
  return check(`${name}_end_code_check`, sql`${table.endCode} is null or (${table.endCode}, ${table.state}) in (${pairs})`);
}
