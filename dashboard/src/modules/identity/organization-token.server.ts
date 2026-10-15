import "@tanstack/react-start/server-only";
import { createHash, randomBytes } from "node:crypto";
import { and, asc, eq, gt, like } from "drizzle-orm";
import { Effect } from "effect";
import type { Caller } from "#/modules/identity/actor";
import { organizationToken, session } from "#/modules/identity/tables";
import { Database } from "#/server/database.server";
import { NotFound, Validation } from "#/server/public-error";

/** Marks a bearer as an Organization Token rather than a session token. */
export const ORGANIZATION_TOKEN_PREFIX = "ployz_";
/** The `ployz` CLI's User-Agent prefix; a session it started is a signed-in device. */
export const CLI_USER_AGENT_PREFIX = "ployz-cli/";

const DAY_MS = 24 * 60 * 60 * 1000;

export function hashOrganizationTokenSecret(secret: string) {
  return createHash("sha256").update(secret).digest("hex");
}

/**
 * Only a signed-in person manages credentials. A token that could mint tokens would outlive its own revocation
 * through the ones it made, and one that could revoke would let a leaked secret lock its Organization out of CI.
 */
const requireSignedIn = (caller: Caller) =>
  caller.credential.kind === "session"
    ? Effect.void
    : Effect.fail(new Validation({ message: "Only a signed-in person can make or revoke tokens; run `ployz login`.", userFacing: true }));

/** Makes a token in the Caller's Organization acting as the Caller. The secret is returned here and never again. */
export const createOrganizationToken = Effect.fn("OrganizationToken.create")(function* (
  caller: Caller,
  input: { readonly name: string; readonly expiresInDays: number },
) {
  yield* requireSignedIn(caller);
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

/** Owners and admins reach every token in the Organization; anyone else only their own. */
const tokensReachableBy = (caller: Caller) => {
  const inOrganization = eq(organizationToken.organizationId, caller.organization.id);
  return caller.role === "owner" || caller.role === "admin" ? inOrganization : and(inOrganization, eq(organizationToken.userId, caller.userId));
};

/** The tokens the Caller can revoke, expired ones included, and the Caller's own signed-in devices. */
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
    .where(tokensReachableBy(caller))
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

/** Revokes a token the Caller can reach or one of the Caller's signed-in devices, at once. */
export const revokeCredential = Effect.fn("OrganizationToken.revoke")(function* (caller: Caller, id: string) {
  yield* requireSignedIn(caller);
  const database = yield* Database;
  const token = yield* database.drizzle
    .delete(organizationToken)
    .where(and(eq(organizationToken.id, id), tokensReachableBy(caller)))
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
