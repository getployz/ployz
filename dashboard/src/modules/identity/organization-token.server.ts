import "@tanstack/react-start/server-only";
import { createHash, randomBytes } from "node:crypto";
import { and, asc, eq, gt, like } from "drizzle-orm";
import { Effect } from "effect";
import type { Caller } from "#/modules/identity/actor";
import { organizationToken, session } from "#/modules/identity/tables";
import { Database } from "#/server/database.server";
import { NotFound } from "#/server/public-error";

/** Marks a bearer as an Organization Token rather than a session token. */
export const ORGANIZATION_TOKEN_PREFIX = "ployz_";
/** The `ployz` CLI's User-Agent prefix; a session it started is a signed-in device. */
export const CLI_USER_AGENT_PREFIX = "ployz-cli/";

const DAY_MS = 24 * 60 * 60 * 1000;

export function hashOrganizationTokenSecret(secret: string) {
  return createHash("sha256").update(secret).digest("hex");
}

/** Makes a token in the Caller's Organization acting as the Caller. The secret is returned here and never again. */
export const createOrganizationToken = Effect.fn("OrganizationToken.create")(function* (
  caller: Caller,
  input: { readonly name: string; readonly expiresInDays: number },
) {
  const database = yield* Database;
  const secret = `${ORGANIZATION_TOKEN_PREFIX}${randomBytes(32).toString("base64url")}`;
  const [created] = yield* database.drizzle
    .insert(organizationToken)
    .values({
      organizationId: caller.organization.id,
      userId: caller.userId,
      name: input.name,
      secretHash: hashOrganizationTokenSecret(secret),
      expiresAt: new Date(Date.now() + input.expiresInDays * DAY_MS),
    })
    .returning({ id: organizationToken.id, expiresAt: organizationToken.expiresAt });
  if (created === undefined) return yield* Effect.die("insert returned no row");
  return {
    id: created.id,
    name: input.name,
    organization: caller.organization.slug,
    expires_at: created.expiresAt.toISOString(),
    secret,
  };
});

/** The Caller's Organization's tokens, expired ones included, and the Caller's own signed-in devices. */
export const listCredentials = Effect.fn("OrganizationToken.list")(function* (caller: Caller) {
  const database = yield* Database;
  const now = new Date();
  const tokens = yield* database.drizzle
    .select({
      id: organizationToken.id,
      name: organizationToken.name,
      createdAt: organizationToken.createdAt,
      expiresAt: organizationToken.expiresAt,
    })
    .from(organizationToken)
    .where(eq(organizationToken.organizationId, caller.organization.id))
    .orderBy(asc(organizationToken.createdAt), asc(organizationToken.id));
  const devices = yield* database.drizzle
    .select({ id: session.id, createdAt: session.createdAt, expiresAt: session.expiresAt })
    .from(session)
    .where(and(
      eq(session.userId, caller.userId),
      like(session.userAgent, `${CLI_USER_AGENT_PREFIX}%`),
      gt(session.expiresAt, now),
    ))
    .orderBy(asc(session.createdAt), asc(session.id));
  return {
    tokens: tokens.map((token) => ({
      id: token.id,
      name: token.name,
      created_at: token.createdAt.toISOString(),
      expires_at: token.expiresAt.toISOString(),
      expired: token.expiresAt <= now,
      current: caller.credential.id === token.id,
    })),
    devices: devices.map((device) => ({
      id: device.id,
      created_at: device.createdAt.toISOString(),
      expires_at: device.expiresAt.toISOString(),
      current: caller.credential.id === device.id,
    })),
  };
});

/** Revokes one of the Organization's tokens or one of the Caller's signed-in devices, at once. */
export const revokeCredential = Effect.fn("OrganizationToken.revoke")(function* (caller: Caller, id: string) {
  const database = yield* Database;
  const token = yield* database.drizzle
    .delete(organizationToken)
    .where(and(eq(organizationToken.id, id), eq(organizationToken.organizationId, caller.organization.id)))
    .returning({ id: organizationToken.id });
  if (token.length > 0) return { id, kind: "token" as const };
  const device = yield* database.drizzle
    .delete(session)
    .where(and(
      eq(session.id, id),
      eq(session.userId, caller.userId),
      like(session.userAgent, `${CLI_USER_AGENT_PREFIX}%`),
    ))
    .returning({ id: session.id });
  if (device.length > 0) return { id, kind: "device" as const };
  return yield* new NotFound({ message: "No such token or signed-in device." });
});
