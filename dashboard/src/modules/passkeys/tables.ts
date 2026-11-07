import { sql } from "drizzle-orm";
import { check, index, pgTable, primaryKey, text, timestamp, uuid } from "drizzle-orm/pg-core";
import { createdAt } from "#/db/tables";
import { user } from "#/modules/identity/tables";
import { organization } from "#/modules/organization/tables";

/**
 * A passkey someone added in the browser, under the code they read out on a Server. Cloud only relays it: a Server
 * trusts the passkey once a Full caller runs `ployz device approve --passkey <code>` there, and only the Servers say
 * whether that happened. The CLI can fetch it for a day; the row stays so the person keeps seeing their passkey.
 */
export const passkeyRequest = pgTable("passkey_request", {
  organizationId: uuid("organization_id").notNull()
    .references(() => organization.id, { onDelete: "cascade" }),
  code: text("code").notNull(),
  userId: uuid("user_id").notNull().references(() => user.id, { onDelete: "cascade" }),
  credentialId: text("credential_id").notNull(),
  algorithm: text("algorithm").notNull(),
  publicKey: text("public_key").notNull(),
  account: text("account").notNull(),
  expiresAt: timestamp("expires_at", { mode: "date", withTimezone: true }).notNull(),
  createdAt,
}, (table) => [
  primaryKey({ columns: [table.organizationId, table.code] }),
  index("passkey_request_user").on(table.organizationId, table.userId),
  check("passkey_request_code_shape", sql`${table.code} ~ '^[a-z2-7]{10}$'`),
  check("passkey_request_algorithm", sql`${table.algorithm} in ('es256', 'eddsa')`),
  check("passkey_request_key_shape", sql`${table.publicKey} ~ '^[A-Za-z0-9_-]+$'`),
]);
