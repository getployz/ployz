import "@tanstack/react-start/server-only";
import { createHash, randomBytes } from "node:crypto";
import { and, asc, eq, gt, isNull } from "drizzle-orm";
import { Effect, Option } from "effect";
import type { Actor } from "#/modules/identity/actor";
import {
  type CreateFleetTokenInput,
  FLEET_TOKEN_PREFIX,
  type FleetTokenLifetime,
  type FleetTokenView,
  type ListFleetTokensInput,
} from "#/modules/machines/waiting/fleet-token";
import { generateTokenName } from "#/modules/machines/waiting/names";
import { fleetToken } from "#/modules/machines/waiting/tables";
import { requireAccess } from "#/modules/organization/access.server";
import { Database, isUniqueViolation } from "#/server/database.server";
import { Conflict, NotFound } from "#/server/public-error";

const DAY_MS = 24 * 60 * 60 * 1000;
const LIFETIME_DAYS = { "1d": 1, "7d": 7, "30d": 30 } satisfies Record<FleetTokenLifetime, number>;
const NAME_ATTEMPTS = 5;

export function hashFleetToken(token: string) {
  return createHash("sha256").update(token).digest("hex");
}

/** Makes a Fleet Token. The token is returned here and never again; Cloud keeps only its hash. */
export const createFleetToken = Effect.fn("FleetToken.create")(function* (
  actor: Actor,
  input: CreateFleetTokenInput,
  generateName: () => string = generateTokenName,
) {
  const { organization } = yield* requireAccess(actor, input.organizationSlug, "operate");
  const { drizzle } = yield* Database;
  const expiresAt = new Date(Date.now() + LIFETIME_DAYS[input.expiresIn] * DAY_MS);
  const insert = (name: string) => {
    const token = `${FLEET_TOKEN_PREFIX}${randomBytes(32).toString("base64url")}`;
    return drizzle
      .insert(fleetToken)
      .values({
        organizationId: organization.id,
        name,
        tokenHash: hashFleetToken(token),
        expiresAt,
        autoEnrollMax: input.autoEnroll?.maxServers ?? null,
        createdByUserId: actor.userId,
      })
      .returning({ id: fleetToken.id })
      .pipe(
        Effect.flatMap(([row]) => row === undefined
          ? Effect.die("Fleet Token insert returned no row")
          : Effect.succeed(Option.some({ token, id: row.id, name, expiresAt: expiresAt.toISOString() }))),
        Effect.catchIf(isUniqueViolation, () => Effect.succeed(Option.none())),
      );
  };
  if (input.name !== null) {
    const created = yield* insert(input.name);
    if (Option.isSome(created)) return created.value;
    return yield* new Conflict({ message: `A Fleet Token named ${input.name} already exists.`, userFacing: true });
  }
  for (let attempt = 0; attempt < NAME_ATTEMPTS; attempt++) {
    const created = yield* insert(generateName());
    if (Option.isSome(created)) return created.value;
  }
  return yield* new Conflict({ message: "Couldn't find a free name; give the token one.", userFacing: true });
});

/** The Organization's unrevoked Fleet Tokens, expired ones included and marked. */
export const listFleetTokens = Effect.fn("FleetToken.list")(function* (actor: Actor, input: ListFleetTokensInput) {
  const { organization } = yield* requireAccess(actor, input.organizationSlug, "operate");
  const { drizzle } = yield* Database;
  const rows = yield* drizzle
    .select({
      id: fleetToken.id,
      name: fleetToken.name,
      expiresAt: fleetToken.expiresAt,
      autoEnrollMax: fleetToken.autoEnrollMax,
      lastUsedAt: fleetToken.lastUsedAt,
      createdAt: fleetToken.createdAt,
    })
    .from(fleetToken)
    .where(and(eq(fleetToken.organizationId, organization.id), isNull(fleetToken.revokedAt)))
    .orderBy(asc(fleetToken.createdAt), asc(fleetToken.id));
  const now = new Date();
  return rows.map((row): FleetTokenView => ({
    id: row.id,
    name: row.name,
    expiresAt: row.expiresAt.toISOString(),
    expired: row.expiresAt <= now,
    autoEnrollMax: row.autoEnrollMax,
    lastUsedAt: row.lastUsedAt?.toISOString() ?? null,
    createdAt: row.createdAt.toISOString(),
  }));
});

/**
 * Revokes a Fleet Token by id, or by the name of a live one. Revoking a revoked token changes nothing. Hosts waiting on
 * the token are not removed yet, so `waitingRemoved` is always 0.
 */
export const revokeFleetToken = Effect.fn("FleetToken.revoke")(function* (
  actor: Actor,
  input: { readonly organizationSlug: string } & ({ readonly id: string } | { readonly name: string }),
) {
  const { organization } = yield* requireAccess(actor, input.organizationSlug, "operate");
  const { drizzle } = yield* Database;
  const target = and(
    eq(fleetToken.organizationId, organization.id),
    "id" in input ? eq(fleetToken.id, input.id) : and(eq(fleetToken.name, input.name), isNull(fleetToken.revokedAt)),
  );
  const [found] = yield* drizzle.select({ id: fleetToken.id }).from(fleetToken).where(target).limit(1);
  if (found === undefined) return yield* new NotFound({ message: "No such Fleet Token." });
  yield* drizzle
    .update(fleetToken)
    .set({ revokedAt: new Date() })
    .where(and(eq(fleetToken.id, found.id), isNull(fleetToken.revokedAt)));
  return { waitingRemoved: 0 };
});

/** The live Fleet Token a host presents, or null when it is unknown, expired or revoked. */
export const loadFleetToken = Effect.fn("FleetToken.load")(function* (token: string) {
  const { drizzle } = yield* Database;
  const [row] = yield* drizzle
    .select({
      id: fleetToken.id,
      organizationId: fleetToken.organizationId,
      name: fleetToken.name,
      expiresAt: fleetToken.expiresAt,
      autoEnrollMax: fleetToken.autoEnrollMax,
    })
    .from(fleetToken)
    .where(and(
      eq(fleetToken.tokenHash, hashFleetToken(token)),
      isNull(fleetToken.revokedAt),
      gt(fleetToken.expiresAt, new Date()),
    ))
    .limit(1);
  return row ?? null;
});
