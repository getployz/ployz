import { sql } from "drizzle-orm";
import { bigint, boolean, index, integer, jsonb, pgTable, text, timestamp, unique, uuid } from "drizzle-orm/pg-core";

import { createdAt, updatedAt } from "#/db/tables";
import { session, user } from "#/modules/identity/tables";

/** better-auth's tables for the hosted MCP plugins (jwt, mcp, cimd) and database rate limiting, from `getAuthTables`. */
export const jwks = pgTable(
  "jwks",
  {
    id: uuid("id").defaultRandom().primaryKey(),
    publicKey: text("public_key").notNull(),
    privateKey: text("private_key").notNull(),
    createdAt: timestamp("created_at", { mode: "date", withTimezone: true }).notNull(),
    expiresAt: timestamp("expires_at", { mode: "date", withTimezone: true }),
    alg: text("alg"),
    crv: text("crv"),
  },
);

export const oauthClient = pgTable(
  "oauth_client",
  {
    id: uuid("id").defaultRandom().primaryKey(),
    clientId: text("client_id").notNull(),
    clientSecret: text("client_secret"),
    clientDiscoveryId: text("client_discovery_id"),
    disabled: boolean("disabled").default(false),
    skipConsent: boolean("skip_consent"),
    enableEndSession: boolean("enable_end_session"),
    subjectType: text("subject_type"),
    scopes: text("scopes").array(),
    clientCredentialsScopes: text("client_credentials_scopes").array().default(sql`'{}'::text[]`),
    userId: uuid("user_id").references(() => user.id, { onDelete: "cascade" }),
    createdAt: timestamp("created_at", { mode: "date", withTimezone: true }),
    updatedAt: timestamp("updated_at", { mode: "date", withTimezone: true }),
    name: text("name"),
    uri: text("uri"),
    icon: text("icon"),
    contacts: text("contacts").array(),
    tos: text("tos"),
    policy: text("policy"),
    softwareId: text("software_id"),
    softwareVersion: text("software_version"),
    softwareStatement: text("software_statement"),
    redirectUris: text("redirect_uris").array().notNull(),
    postLogoutRedirectUris: text("post_logout_redirect_uris").array(),
    backchannelLogoutUri: text("backchannel_logout_uri"),
    backchannelLogoutSessionRequired: boolean("backchannel_logout_session_required"),
    tokenEndpointAuthMethod: text("token_endpoint_auth_method"),
    applicationType: text("application_type"),
    jwks: text("jwks"),
    jwksUri: text("jwks_uri"),
    grantTypes: text("grant_types").array(),
    responseTypes: text("response_types").array(),
    requirePKCE: boolean("require_pkce"),
    dpopBoundAccessTokens: boolean("dpop_bound_access_tokens").default(false),
    referenceId: text("reference_id"),
    metadata: jsonb("metadata"),
  },
  (table) => [
    unique().on(table.clientId),
    index("oauth_client_user_id_idx").on(table.userId),
  ],
);

export const oauthResource = pgTable(
  "oauth_resource",
  {
    id: uuid("id").defaultRandom().primaryKey(),
    identifier: text("identifier").notNull(),
    name: text("name").notNull(),
    accessTokenTtl: integer("access_token_ttl"),
    refreshTokenTtl: integer("refresh_token_ttl"),
    signingAlgorithm: text("signing_algorithm"),
    signingKeyId: text("signing_key_id"),
    allowedScopes: text("allowed_scopes").array(),
    customClaims: jsonb("custom_claims"),
    dpopBoundAccessTokensRequired: boolean("dpop_bound_access_tokens_required").default(false),
    disabled: boolean("disabled").default(false),
    createdAt: timestamp("created_at", { mode: "date", withTimezone: true }),
    updatedAt: timestamp("updated_at", { mode: "date", withTimezone: true }),
    policyVersion: integer("policy_version").default(1),
    metadata: jsonb("metadata"),
  },
  (table) => [
    unique().on(table.identifier),
  ],
);

export const oauthClientResource = pgTable(
  "oauth_client_resource",
  {
    id: uuid("id").defaultRandom().primaryKey(),
    clientId: text("client_id").notNull().references(() => oauthClient.clientId, { onDelete: "cascade" }),
    resourceId: text("resource_id").notNull().references(() => oauthResource.identifier, { onDelete: "cascade" }),
    metadata: jsonb("metadata"),
    createdAt: timestamp("created_at", { mode: "date", withTimezone: true }),
  },
  (table) => [
    index("oauth_client_resource_client_id_idx").on(table.clientId),
    index("oauth_client_resource_resource_id_idx").on(table.resourceId),
    unique().on(table.clientId, table.resourceId),
  ],
);

export const oauthRefreshToken = pgTable(
  "oauth_refresh_token",
  {
    id: uuid("id").defaultRandom().primaryKey(),
    createdAt,
    token: text("token").notNull(),
    clientId: text("client_id").notNull().references(() => oauthClient.clientId),
    sessionId: uuid("session_id").references(() => session.id, { onDelete: "set null" }),
    userId: uuid("user_id").notNull().references(() => user.id, { onDelete: "cascade" }),
    referenceId: text("reference_id"),
    authorizationCodeId: text("authorization_code_id"),
    resources: text("resources").array(),
    requestedUserInfoClaims: text("requested_user_info_claims").array(),
    expiresAt: timestamp("expires_at", { mode: "date", withTimezone: true }).notNull(),
    revoked: timestamp("revoked", { mode: "date", withTimezone: true }),
    rotatedAt: timestamp("rotated_at", { mode: "date", withTimezone: true }),
    rotationReplayResponse: text("rotation_replay_response"),
    rotationReplayExpiresAt: timestamp("rotation_replay_expires_at", { mode: "date", withTimezone: true }),
    authTime: timestamp("auth_time", { mode: "date", withTimezone: true }),
    confirmation: jsonb("confirmation"),
    scopes: text("scopes").array().notNull(),
  },
  (table) => [
    unique().on(table.token),
    index("oauth_refresh_token_client_id_idx").on(table.clientId),
    index("oauth_refresh_token_session_id_idx").on(table.sessionId),
    index("oauth_refresh_token_user_id_idx").on(table.userId),
    index("oauth_refresh_token_authorization_code_id_idx").on(table.authorizationCodeId),
  ],
);

export const oauthAccessToken = pgTable(
  "oauth_access_token",
  {
    id: uuid("id").defaultRandom().primaryKey(),
    createdAt,
    token: text("token").notNull(),
    clientId: text("client_id").notNull().references(() => oauthClient.clientId),
    sessionId: uuid("session_id").references(() => session.id, { onDelete: "set null" }),
    userId: uuid("user_id").references(() => user.id, { onDelete: "cascade" }),
    referenceId: text("reference_id"),
    authorizationCodeId: text("authorization_code_id"),
    resources: text("resources").array(),
    requestedUserInfoClaims: text("requested_user_info_claims").array(),
    refreshId: uuid("refresh_id").references(() => oauthRefreshToken.id),
    expiresAt: timestamp("expires_at", { mode: "date", withTimezone: true }).notNull(),
    revoked: timestamp("revoked", { mode: "date", withTimezone: true }),
    confirmation: jsonb("confirmation"),
    scopes: text("scopes").array().notNull(),
  },
  (table) => [
    unique().on(table.token),
    index("oauth_access_token_client_id_idx").on(table.clientId),
    index("oauth_access_token_session_id_idx").on(table.sessionId),
    index("oauth_access_token_user_id_idx").on(table.userId),
    index("oauth_access_token_authorization_code_id_idx").on(table.authorizationCodeId),
    index("oauth_access_token_refresh_id_idx").on(table.refreshId),
  ],
);

export const oauthConsent = pgTable(
  "oauth_consent",
  {
    id: uuid("id").defaultRandom().primaryKey(),
    createdAt,
    updatedAt,
    clientId: text("client_id").notNull().references(() => oauthClient.clientId),
    userId: uuid("user_id").references(() => user.id, { onDelete: "cascade" }),
    referenceId: text("reference_id"),
    resources: text("resources").array(),
    requestedUserInfoClaims: text("requested_user_info_claims").array(),
    scopes: text("scopes").array().notNull(),
  },
  (table) => [
    index("oauth_consent_client_id_idx").on(table.clientId),
    index("oauth_consent_user_id_idx").on(table.userId),
    unique().on(table.clientId, table.userId, table.referenceId),
  ],
);

export const oauthClientAssertion = pgTable(
  "oauth_client_assertion",
  {
    // The id is a digest of the client's jti, not a uuid.
    id: text("id").primaryKey(),
    expiresAt: timestamp("expires_at", { mode: "date", withTimezone: true }).notNull(),
  },
);

export const rateLimit = pgTable(
  "rate_limit",
  {
    id: uuid("id").defaultRandom().primaryKey(),
    key: text("key").notNull(),
    count: integer("count").notNull(),
    lastRequest: bigint("last_request", { mode: "number" }).notNull(),
  },
  (table) => [
    unique().on(table.key),
  ],
);
