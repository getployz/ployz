import { createdAt } from "#/db/tables";
import { user } from "#/modules/identity/tables";
import type { InviteRole, MemberRole } from "#/modules/organization/permissions";

import { createSelectSchema } from "drizzle-orm/effect-schema";

import { index, pgTable, text, timestamp, unique, uuid } from "drizzle-orm/pg-core";

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
    role: text("role").default("member").notNull().$type<MemberRole>(),
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
    role: text("role").notNull().$type<InviteRole>(),
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
