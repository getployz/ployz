import { sql } from "drizzle-orm";
import { createdAt, sqlStringLiterals, updatedAt } from "#/db/tables";
import { TOKEN_KINDS, type TokenKind } from "#/modules/identity/actor";
import type { Permission } from "#/modules/organization/permissions";

import { organization } from "#/modules/organization/tables";

import { boolean, check, index, integer, pgTable, text, timestamp, unique, uuid } from "drizzle-orm/pg-core";



export const user = pgTable(
  "user",
  {
    id: uuid("id").defaultRandom().primaryKey(),
    createdAt,
    updatedAt,
    email: text("email").notNull(),
    emailVerified: boolean("email_verified").default(false).notNull(),
    name: text("name").notNull(),
    image: text("image"),
    openStartedDeployments: boolean("open_started_deployments").default(true).notNull(),
    isAnonymous: boolean("is_anonymous").default(false).notNull(),
  },
  (table) => [unique().on(table.email)],
);

export const session = pgTable(
  "session",
  {
    id: uuid("id").defaultRandom().primaryKey(),
    createdAt,
    updatedAt,
    userId: uuid("user_id")
      .notNull()
      .references(() => user.id, { onDelete: "cascade" }),
    expiresAt: timestamp("expires_at", {
      mode: "date",
      withTimezone: true,
    }).notNull(),
    token: text("token").notNull(),
    activeOrganizationId: uuid("active_organization_id"),
    activeOrganizationSlug: text("active_organization_slug"),
    ipAddress: text("ip_address"),
    userAgent: text("user_agent"),
  },
  (table) => [
    unique().on(table.token),
    index("session_user_id_idx").on(table.userId),
  ],
);

export const account = pgTable(
  "account",
  {
    id: uuid("id").defaultRandom().primaryKey(),
    createdAt,
    updatedAt,
    accountId: text("account_id").notNull(),
    providerId: text("provider_id").notNull(),
    userId: uuid("user_id")
      .notNull()
      .references(() => user.id, { onDelete: "cascade" }),
    accessToken: text("access_token"),
    refreshToken: text("refresh_token"),
    idToken: text("id_token"),
    accessTokenExpiresAt: timestamp("access_token_expires_at", {
      mode: "date",
      withTimezone: true,
    }),
    refreshTokenExpiresAt: timestamp("refresh_token_expires_at", {
      mode: "date",
      withTimezone: true,
    }),
    scope: text("scope"),
    password: text("password"),
  },
  (table) => [
    index("account_user_id_idx").on(table.userId),
    index("account_provider_account_idx").on(table.providerId, table.accountId),
  ],
);

export const verification = pgTable(
  "verification",
  {
    // better-auth reserves one-shot values (DPoP proof ids) under a digest id, not a uuid.
    id: text("id").default(sql`gen_random_uuid()::text`).primaryKey(),
    createdAt,
    updatedAt,
    identifier: text("identifier").notNull(),
    value: text("value").notNull(),
    expiresAt: timestamp("expires_at", {
      mode: "date",
      withTimezone: true,
    }).notNull(),
  },
  (table) => [index("verification_identifier_idx").on(table.identifier)],
);

/** better-auth's RFC 8628 device codes: one per pending `ployz login`, claimed by the user who opens it. */
export const deviceCode = pgTable(
  "device_code",
  {
    id: uuid("id").defaultRandom().primaryKey(),
    createdAt,
    updatedAt,
    deviceCode: text("device_code").notNull(),
    userCode: text("user_code").notNull(),
    userId: uuid("user_id").references(() => user.id, { onDelete: "cascade" }),
    expiresAt: timestamp("expires_at", {
      mode: "date",
      withTimezone: true,
    }).notNull(),
    status: text("status").notNull().$type<"pending" | "approved" | "denied">(),
    lastPolledAt: timestamp("last_polled_at", {
      mode: "date",
      withTimezone: true,
    }),
    pollingInterval: integer("polling_interval"),
    clientId: text("client_id"),
    scope: text("scope"),
  },
  (table) => [unique().on(table.deviceCode), unique().on(table.userCode)],
);

/**
 * Organization Tokens: `PLOYZ_TOKEN` credentials, each bound to one Organization and acting as the member who made it.
 * Only the SHA-256 of the secret is kept; the secret is shown once, when the token is made.
 */
export const organizationToken = pgTable(
  "organization_token",
  {
    id: uuid("id").defaultRandom().primaryKey(),
    createdAt,
    organizationId: uuid("organization_id")
      .notNull()
      .references(() => organization.id, { onDelete: "cascade" }),
    userId: uuid("user_id")
      .notNull()
      .references(() => user.id, { onDelete: "cascade" }),
    name: text("name").notNull(),
    kind: text("kind").$type<TokenKind>().notNull().default("cli"),
    secretHash: text("secret_hash").notNull(),
    expiresAt: timestamp("expires_at", {
      mode: "date",
      withTimezone: true,
    }).notNull(),
    permissions: text("permissions").$type<Permission>().array(),
    environments: text("environments").array(),
  },
  (table) => [
    unique().on(table.secretHash),
    index("organization_token_organization_id_idx").on(table.organizationId),
    check("organization_token_kind_check", sql`${table.kind} in (${sqlStringLiterals(TOKEN_KINDS)})`),
  ],
);
