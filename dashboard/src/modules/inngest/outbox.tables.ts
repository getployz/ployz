import { sql } from "drizzle-orm";
import { index, jsonb, pgTable, text, timestamp } from "drizzle-orm/pg-core";
import { createdAt } from "#/db/tables";

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
