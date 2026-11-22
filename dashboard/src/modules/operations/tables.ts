import { sql } from "drizzle-orm";
import { check, pgTable, primaryKey, text, uuid } from "drizzle-orm/pg-core";
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
