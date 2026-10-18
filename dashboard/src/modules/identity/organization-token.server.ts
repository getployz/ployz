import "@tanstack/react-start/server-only";
import { createHash, randomBytes } from "node:crypto";
import { and, asc, eq, gt, like } from "drizzle-orm";
import { Effect } from "effect";
import { type Caller, callerCan, type TokenKind } from "#/modules/identity/actor";
import { readStore } from "#/modules/config-store/config-store.server";
import { requireClaimed } from "#/modules/identity/claim-required.server";
import { organizationToken, session } from "#/modules/identity/tables";
import { FULL_GRANT, grantLabel, presetGrant, type TokenGrant, type TokenPreset } from "#/modules/identity/token-grant";
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
export const requireSignedIn = (caller: Caller) =>
  caller.credential.kind === "session"
    ? Effect.void
    : Effect.fail(new Validation({ message: "Only a signed-in person can make or revoke tokens; run `ployz login`.", userFacing: true }));

export const mintOrganizationTokenUnchecked = Effect.fn("OrganizationToken.mint")(function* (input: {
  readonly organizationId: string;
  readonly userId: string;
  readonly name: string;
  readonly kind: TokenKind;
  readonly expiresAt: Date;
  readonly grant?: TokenGrant;
}) {
  const database = yield* Database;
  const secret = `${ORGANIZATION_TOKEN_PREFIX}${randomBytes(32).toString("base64url")}`;
  const { grant = FULL_GRANT, ...row } = input;
  const [created] = yield* database.drizzle
    .insert(organizationToken)
    .values({
      ...row,
      permissions: grant.permissions === null ? null : [...grant.permissions],
      environments: grant.environments === null ? null : [...grant.environments],
      secretHash: hashOrganizationTokenSecret(secret),
    })
    .returning({ id: organizationToken.id, kind: organizationToken.kind, expiresAt: organizationToken.expiresAt });
  if (created === undefined) return yield* Effect.die("insert returned no row");
  return { id: created.id, kind: created.kind, expiresAt: created.expiresAt, secret };
});

/** Makes a token in the Caller's Organization acting as the Caller. The secret is returned here and never again. */
export const createOrganizationToken = Effect.fn("OrganizationToken.create")(function* (
  caller: Caller,
  input: { readonly name: string; readonly expiresInDays: number; readonly kind?: TokenKind; readonly preset?: TokenPreset },
) {
  yield* requireClaimed(caller.organization.id, "mint_token");
  yield* requireSignedIn(caller);
  const grant = presetGrant(input.preset ?? { preset: "full" });
  const minted = yield* mintOrganizationTokenUnchecked({
    organizationId: caller.organization.id,
    userId: caller.userId,
    name: input.name,
    kind: caller.via === "agent" ? "agent" : (input.kind ?? "cli"),
    expiresAt: new Date(Date.now() + input.expiresInDays * DAY_MS),
    grant,
  });
  return {
    id: minted.id,
    name: input.name,
    kind: minted.kind,
    organization: caller.organization.slug,
    expires_at: minted.expiresAt.toISOString(),
    scope: grantLabel(grant),
    secret: minted.secret,
  };
});

/**
 * A Deploy token's Environment, refused when no Project in the Organization has one by that name, or when one by that
 * name is Protected and the Caller can't protect it: the token would deploy where its maker only asks.
 */
export const knownEnvironment = Effect.fn("OrganizationToken.knownEnvironment")(function* (caller: Caller, preset: TokenPreset) {
  if (preset.preset !== "deploy") return preset;
  const { projects } = yield* readStore(caller.organization.id, { query: "projects" });
  const named = projects.filter((project) => project.environments.includes(preset.environment));
  if (named.length === 0) {
    return yield* new Validation({ message: `No Project here has an Environment named ${preset.environment}.`, userFacing: true });
  }
  if (callerCan(caller, "environments:protect")) return preset;
  for (const project of named) {
    const { environments } = yield* readStore(caller.organization.id, { query: "environments", project: project.name });
    if (environments.some((environment) => environment.name === preset.environment && environment.protected)) {
      return yield* new Validation({
        message: `${preset.environment} is Protected; only an owner or admin can make a Deploy token for it.`,
        userFacing: true,
      });
    }
  }
  return preset;
});

/** Owners and admins reach every token in the Organization; anyone else only their own. */
const tokensReachableBy = (caller: Caller) => {
  const inOrganization = eq(organizationToken.organizationId, caller.organization.id);
  return callerCan(caller, "tokens:manage") ? inOrganization : and(inOrganization, eq(organizationToken.userId, caller.userId));
};

/** The tokens the Caller can revoke, expired ones included, and the Caller's own signed-in devices. */
export const listCredentials = Effect.fn("OrganizationToken.list")(function* (caller: Caller) {
  const database = yield* Database;
  const now = new Date();
  const tokens = yield* database.drizzle
    .select({
      id: organizationToken.id,
      name: organizationToken.name,
      kind: organizationToken.kind,
      createdAt: organizationToken.createdAt,
      expiresAt: organizationToken.expiresAt,
      permissions: organizationToken.permissions,
      environments: organizationToken.environments,
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
      kind: token.kind,
      scope: grantLabel({ permissions: token.permissions, environments: token.environments }),
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
