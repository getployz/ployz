import "@tanstack/react-start/server-only";
import { and, asc, count, eq, inArray } from "drizzle-orm";
import type { AnyPgColumn, PgTable } from "drizzle-orm/pg-core";
import { Effect } from "effect";
import { cloudStore, storeTry } from "#/modules/config-store/store-sdk.server";
import { readStore } from "#/modules/config-store/config-store.server";
import type { Actor } from "#/modules/identity/actor";
import { claimSecretFor, hashClaimSecret } from "#/modules/identity/claim-required.server";
import { invitation, member, organizationToken, session, user } from "#/modules/identity/tables";
import { countOrganizationMachines } from "#/modules/machines/connections.server";
import { machineEnrollmentToken, machineRemoveAttempt, serverAccess } from "#/modules/machines/tables";
import { fleetToken } from "#/modules/machines/waiting/tables";
import { personalOrganizationName } from "#/modules/organization/organization-state";
import { updateActorSessionsOrganization } from "#/modules/organization/organization-state.server";
import { organization } from "#/modules/organization/tables";
import { organizationPairing } from "#/modules/runtime/tables";
import { Database } from "#/server/database.server";
import { Conflict, NotFound } from "#/server/public-error";

/** `ployz login` tokens default to 90 days; the claimed agent's token gets the same. */
const CLAIMED_TOKEN_TTL_MS = 90 * 24 * 60 * 60 * 1000;

/** What the claim page shows for a secret, to a viewer who may be signed in. */
export type Claim =
  | {
    readonly state: "pending";
    readonly organization: { readonly name: string; readonly slug: string };
    readonly servers: number;
    readonly projects: number;
    readonly expiresAt: Date;
  }
  | { readonly state: "claimed"; readonly organization: { readonly slug: string } }
  | { readonly state: "expired" };

const spent = () => new NotFound({ message: "This claim link is spent or unknown." });

/** The Organization still waiting for a person under this secret, with its anonymous owners and when it expires. */
const findPendingOrganization = Effect.fn("Identity.findPendingOrganization")(function* (secret: string) {
  const { drizzle } = yield* Database;
  const [found] = yield* drizzle
    .select({ id: organization.id, name: organization.name, slug: organization.slug })
    .from(organization)
    .where(eq(organization.claimSecretHash, hashClaimSecret(secret)))
    .limit(1);
  if (found === undefined) return null;
  const owners = yield* drizzle
    .select({ userId: member.userId })
    .from(member)
    .innerJoin(user, eq(user.id, member.userId))
    .where(and(eq(member.organizationId, found.id), eq(member.role, "owner"), eq(user.isAnonymous, true)));
  const anonymousIds = owners.map((owner) => owner.userId);
  if (anonymousIds.length === 0) return null;
  const tokens = yield* drizzle
    .select({ expiresAt: organizationToken.expiresAt })
    .from(organizationToken)
    .where(and(eq(organizationToken.organizationId, found.id), inArray(organizationToken.userId, anonymousIds)));
  const expiresAt = tokens.reduce<Date | null>((latest, token) => latest === null || token.expiresAt > latest ? token.expiresAt : latest, null);
  return { ...found, anonymousIds, expiresAt };
});

/** The Organization the viewer already claimed under this secret: the secret derives from the id, so no hash is needed. */
const findClaimedOrganization = Effect.fn("Identity.findClaimedOrganization")(function* (viewer: Actor, secret: string) {
  const { drizzle } = yield* Database;
  const memberships = yield* drizzle
    .select({ id: organization.id, slug: organization.slug })
    .from(member)
    .innerJoin(organization, eq(organization.id, member.organizationId))
    .where(eq(member.userId, viewer.userId));
  for (const membership of memberships) {
    if ((yield* claimSecretFor(membership.id)) === secret) return membership;
  }
  return null;
});

/** A sandbox with no live anonymous token has lapsed: nobody can act in it until it is claimed, and teardown is near. */
const live = (expiresAt: Date | null): expiresAt is Date => expiresAt !== null && expiresAt.getTime() > Date.now();

export const readClaim = Effect.fn("Identity.readClaim")(function* (secret: string, viewer: Actor | null) {
  const pending = yield* findPendingOrganization(secret);
  if (pending !== null) {
    if (!live(pending.expiresAt)) return { state: "expired" } satisfies Claim;
    const [servers, { projects }] = yield* Effect.all([countOrganizationMachines(pending.id), readStore(pending.id, { query: "projects" })]);
    return {
      state: "pending",
      organization: { name: pending.name, slug: pending.slug },
      servers,
      projects: projects.length,
      expiresAt: pending.expiresAt,
    } satisfies Claim;
  }
  const claimed = viewer === null ? null : yield* findClaimedOrganization(viewer, secret);
  return (claimed === null ? { state: "expired" } : { state: "claimed", organization: { slug: claimed.slug } }) satisfies Claim;
});

/**
 * The person keeps the Organization: they become its sole owner, every row the anonymous agent made now names them,
 * the agent's token runs 90 more days, and the anonymous user goes. Their own empty auto-created Organization goes
 * too, and their sessions act in the claimed one. Claiming again answers the same slug.
 */
export const claimOrganization = Effect.fn("Identity.claimOrganization")(function* (actor: Actor, secret: string) {
  const database = yield* Database;
  const claimed = yield* database.transaction(Effect.gen(function* () {
    const { drizzle } = yield* Database;
    const [locked] = yield* drizzle
      .select({ id: organization.id })
      .from(organization)
      .where(eq(organization.claimSecretHash, hashClaimSecret(secret)))
      .for("update");
    if (locked === undefined) {
      const already = yield* findClaimedOrganization(actor, secret);
      return already === null ? yield* spent() : { slug: already.slug, kind: "repeat" as const };
    }
    yield* drizzle.select({ userId: member.userId }).from(member)
      .where(eq(member.organizationId, locked.id)).orderBy(asc(member.userId)).for("update");
    const pending = yield* findPendingOrganization(secret);
    if (pending === null) return yield* spent();
    if (!live(pending.expiresAt)) return yield* new Conflict({ message: "This sandbox expired.", userFacing: true });
    const { id, anonymousIds } = pending;
    yield* drizzle.insert(member).values({ userId: actor.userId, organizationId: id, role: "owner" })
      .onConflictDoUpdate({ target: [member.userId, member.organizationId], set: { role: "owner" } });
    yield* drizzle.update(organizationToken)
      .set({ userId: actor.userId, expiresAt: new Date(Date.now() + CLAIMED_TOKEN_TTL_MS) })
      .where(and(eq(organizationToken.organizationId, id), inArray(organizationToken.userId, anonymousIds)));
    yield* drizzle.update(serverAccess).set({ userId: actor.userId })
      .where(and(eq(serverAccess.organizationId, id), inArray(serverAccess.userId, anonymousIds)));
    yield* drizzle.update(machineRemoveAttempt).set({ requestedByUserId: actor.userId })
      .where(inArray(machineRemoveAttempt.requestedByUserId, anonymousIds));
    yield* drizzle.update(machineEnrollmentToken).set({ createdByUserId: actor.userId })
      .where(inArray(machineEnrollmentToken.createdByUserId, anonymousIds));
    yield* drizzle.update(fleetToken).set({ createdByUserId: actor.userId })
      .where(inArray(fleetToken.createdByUserId, anonymousIds));
    yield* drizzle.update(organization).set({ claimSecretHash: null }).where(eq(organization.id, id));
    yield* drizzle.delete(user).where(inArray(user.id, anonymousIds));
    return { id, slug: pending.slug, kind: "claimed" as const };
  }));
  if (claimed.kind === "claimed") {
    yield* removeEmptyPersonalOrganizations(actor, claimed.id);
    yield* updateActorSessionsOrganization(actor, claimed.id, claimed.slug);
  }
  return { slug: claimed.slug };
});

/**
 * The Organization sign-in made for the claimer, if nothing ever happened in it: they are its only member, it still
 * carries its auto-created name, and it has no Pairing, token, invite or Project. A returning person keeps theirs.
 */
const removeEmptyPersonalOrganizations = Effect.fn("Identity.removeEmptyPersonalOrganizations")(function* (actor: Actor, keep: string) {
  const database = yield* Database;
  const { drizzle } = database;
  const [person] = yield* drizzle.select({ id: user.id, name: user.name, email: user.email }).from(user).where(eq(user.id, actor.userId));
  if (person === undefined) return;
  const candidates = yield* drizzle
    .select({ id: organization.id })
    .from(member)
    .innerJoin(organization, eq(organization.id, member.organizationId))
    .where(and(eq(member.userId, actor.userId), eq(organization.name, personalOrganizationName(person))));
  const store = yield* cloudStore;
  for (const { id } of candidates) {
    if (id === keep || !(yield* isUntouched(id))) continue;
    const refused = yield* storeTry(() => store.removeOrganization(id)).pipe(
      Effect.as(false),
      Effect.catchTag("StoreRefused", () => Effect.succeed(true)),
    );
    if (refused) continue;
    yield* database.transaction(Effect.gen(function* () {
      const { drizzle: transaction } = yield* Database;
      yield* transaction.delete(organization).where(eq(organization.id, id));
      yield* transaction.update(session).set({ activeOrganizationId: null, activeOrganizationSlug: null })
        .where(eq(session.activeOrganizationId, id));
    }));
  }
});

const isUntouched = Effect.fn("Identity.isUntouchedOrganization")(function* (organizationId: string) {
  const { drizzle } = yield* Database;
  const rowsIn = (table: PgTable, organizationColumn: AnyPgColumn) =>
    drizzle.select({ rows: count() }).from(table).where(eq(organizationColumn, organizationId)).pipe(
      Effect.map((result) => result[0]?.rows ?? 0),
    );
  const [members, ...others] = yield* Effect.all([
    rowsIn(member, member.organizationId),
    rowsIn(organizationPairing, organizationPairing.organizationId),
    rowsIn(organizationToken, organizationToken.organizationId),
    rowsIn(invitation, invitation.organizationId),
    rowsIn(fleetToken, fleetToken.organizationId),
    rowsIn(machineEnrollmentToken, machineEnrollmentToken.organizationId),
  ]);
  if (members !== 1 || others.some((rows) => rows !== 0)) return false;
  const { projects } = yield* readStore(organizationId, { query: "projects" });
  return projects.length === 0;
});
