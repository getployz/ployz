import { sql } from "drizzle-orm";
import { check, index, jsonb, pgTable, primaryKey, text, timestamp, uuid } from "drizzle-orm/pg-core";
import { createdAt } from "#/db/tables";
import { organization } from "#/modules/organization/tables";

/** The attempt a request key admitted, so the same request again answers it instead of starting another. */
export const operationRequest = pgTable(
  "operation_request",
  {
    organizationId: uuid("organization_id")
      .notNull()
      .references(() => organization.id, { onDelete: "cascade" }),
    requestKey: text("request_key").notNull(),
    tool: text("tool").notNull(),
    digest: text("digest").notNull(),
    attemptId: text("attempt_id").notNull(),
    createdAt,
  },
  (table) => [
    primaryKey({ columns: [table.organizationId, table.requestKey] }),
    check("operation_request_key_check", sql`${table.requestKey} ~ '^[A-Za-z0-9_-]{8,64}$'`),
  ],
);

/** An event written with the row it starts, sent after the commit and again until Inngest takes it. */
export const operationOutbox = pgTable(
  "operation_outbox",
  {
    eventId: text("event_id").primaryKey(),
    name: text("name").notNull(),
    data: jsonb("data").notNull(),
    createdAt,
    sentAt: timestamp("sent_at", { mode: "date", withTimezone: true }),
  },
  (table) => [
    index("operation_outbox_unsent_idx").on(table.createdAt).where(sql`${table.sentAt} is null`),
  ],
);
