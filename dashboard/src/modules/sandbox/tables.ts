import { organization } from "#/modules/organization/tables";
import { sql } from "drizzle-orm";
import { pgTable, timestamp, uuid } from "drizzle-orm/pg-core";

/**
 * A lapsed sandbox whose removal waits on its Servers, timed by the database clock. `silentSince` is the first probe
 * that reached none of them since the last answer; it counts only from the current pairing's creation and its last
 * removal answer. `warnedAt` is the one warning that it has waited a day.
 */
export const anonymousTeardown = pgTable("anonymous_teardown", {
  organizationId: uuid("organization_id").primaryKey().references(() => organization.id, { onDelete: "cascade" }),
  waitingSince: timestamp("waiting_since", { mode: "date", withTimezone: true }).notNull().default(sql`clock_timestamp()`),
  silentSince: timestamp("silent_since", { mode: "date", withTimezone: true }),
  warnedAt: timestamp("warned_at", { mode: "date", withTimezone: true }),
});
