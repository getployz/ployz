import { createdAt } from "#/db/tables";
import { user } from "#/modules/identity/tables";

import { createSelectSchema } from "drizzle-orm/effect-schema";

import { sql } from "drizzle-orm";

import { bigint, boolean, customType, index, pgTable, text, timestamp, unique, uuid } from "drizzle-orm/pg-core";

import { Schema } from "effect";



const requiredTrimmedString = Schema.Trim.check(Schema.isNonEmpty());

export const organization = pgTable(
  "organization",
  {
    id: uuid("id").defaultRandom().primaryKey(),
    name: text("name").notNull(),
    slug: text("slug").notNull(),
    logo: text("logo"),
    metadata: text("metadata"),
    /** sha256 hex of the claim secret of an Organization an anonymous agent made, kept after a person claims it so the link says so. */
    claimSecretHash: text("claim_secret_hash"),
    createdAt,
  },
  (table) => [unique().on(table.slug)],
);

const organizationSelectSchema = createSelectSchema(organization, {
  name: requiredTrimmedString,
  slug: requiredTrimmedString,
});

export const organizationSlugSchema = organizationSelectSchema.fields.slug;

const xid8 = customType<{ data: string }>({ dataType: () => "xid8" });

/**
 * Organization change log, written only by the `organization_change_log` statement trigger.
 * Readers see a row once its transaction and every older one has finished:
 * `xid < pg_snapshot_xmin(pg_current_snapshot())`. That xid horizon is the read cursor.
 * No organization FK: cascading organization deletes still log their rows.
 */
export const organizationChange = pgTable(
  "organization_change",
  {
    seq: bigint("seq", { mode: "number" }).primaryKey().generatedAlwaysAsIdentity(),
    xid: xid8("xid").notNull().default(sql`pg_current_xact_id()`),
    organizationId: uuid("organization_id").notNull(),
    sourceTable: text("source_table").notNull(),
    changedIds: text("changed_ids").array().notNull(),
    deletedIds: text("deleted_ids").array().notNull(),
    // More than 100 keys in one statement: the ids are dropped and readers do a full read.
    allRows: boolean("all_rows").notNull(),
    createdAt,
  },
  (table) => [
    index("organization_change_organization_id_xid_idx").on(table.organizationId, table.xid),
    // The oldest xid is the retention fence.
    index("organization_change_xid_idx").on(table.xid),
  ],
);

export const member = pgTable(
  "member",
  {
    id: uuid("id").defaultRandom().primaryKey(),
    userId: uuid("user_id")
      .notNull()
      .references(() => user.id, { onDelete: "cascade" }),
    organizationId: uuid("organization_id")
      .notNull()
      .references(() => organization.id, { onDelete: "cascade" }),
    role: text("role").default("member").notNull().$type<"owner" | "admin" | "member">(),
    createdAt,
  },
  (table) => [unique().on(table.userId, table.organizationId)],
);

/**
 * A link that makes whoever opens it a member. Only the token's hash is kept; the link is shown once. It lasts 7 days
 * and is used once. `email` is a label for the inviter, not who may accept. Expired is read from `expires_at`.
 */
export const invitation = pgTable(
  "invitation",
  {
    id: uuid("id").defaultRandom().primaryKey(),
    organizationId: uuid("organization_id")
      .notNull()
      .references(() => organization.id, { onDelete: "cascade" }),
    tokenHash: text("token_hash").notNull().unique(),
    email: text("email"),
    role: text("role").notNull().$type<"admin" | "member">(),
    status: text("status").default("pending").notNull().$type<"pending" | "accepted" | "revoked">(),
    expiresAt: timestamp("expires_at", {
      mode: "date",
      withTimezone: true,
    }).notNull(),
    inviterId: uuid("inviter_id")
      .notNull()
      .references(() => user.id, { onDelete: "cascade" }),
    acceptedByUserId: uuid("accepted_by_user_id").references(() => user.id, { onDelete: "set null" }),
    createdAt,
  },
  (table) => [
    index("invitation_organization_id_idx").on(table.organizationId),
    index("invitation_email_idx").on(table.email),
  ],
);
