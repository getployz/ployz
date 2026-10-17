import "@tanstack/react-start/server-only";
import { and, asc, count, eq, inArray, ne, sql } from "drizzle-orm";
import type { AnyPgColumn, PgTable } from "drizzle-orm/pg-core";
import { Effect } from "effect";
import { readStore } from "#/modules/config-store/config-store.server";
import { cloudStore } from "#/modules/config-store/store-sdk.server";
import type { Actor } from "#/modules/identity/actor";
import { hashClaimSecret } from "#/modules/identity/claim-required.server";
import { invitation, member, organizationToken, user } from "#/modules/identity/tables";
import { organizationBillingState } from "#/modules/billing/tables";
import { organizationGithubInstallation } from "#/modules/github/tables";
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
  | { readonly state: "taken" }
  | { readonly state: "expired" };

const unknown = () => new NotFound({ message: "This claim link is unknown." });
const taken = () => new Conflict({ message: "Someone already kept this sandbox.", userFacing: true });

/** The Organization under this secret, its anonymous owners (none once a person kept it), and when their tokens lapse. */
const findBySecret = Effect.fn("Identity.findClaimOrganization")(function* (secret: string) {
  const { drizzle } = yield* Database;
  const [found] = yield* drizzle
    .select({ id: organization.id, name: organization.name, slug: organization.slug, createdAt: organization.createdAt })
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
  const tokens = anonymousIds.length === 0 ? [] : yield* drizzle
    .select({ expiresAt: organizationToken.expiresAt })
    .from(organizationToken)
    .where(and(eq(organizationToken.organizationId, found.id), inArray(organizationToken.userId, anonymousIds)));
  const expiresAt = tokens.reduce<Date | null>((latest, token) => latest === null || token.expiresAt > latest ? token.expiresAt : latest, null);
  return { ...found, anonymousIds, expiresAt };
});

const isMember = Effect.fn("Identity.isClaimMember")(function* (actor: Actor, organizationId: string) {
  const { drizzle } = yield* Database;
  const rows = yield* drizzle.select({ userId: member.userId }).from(member)
    .where(and(eq(member.organizationId, organizationId), eq(member.userId, actor.userId)));
  return rows.length > 0;
});

/** A sandbox with no live anonymous token has lapsed: nobody can act in it until it is claimed, and teardown is near. */
const live = (expiresAt: Date | null): expiresAt is Date => expiresAt !== null && expiresAt.getTime() > Date.now();

export const readClaim = Effect.fn("Identity.readClaim")(function* (secret: string, viewer: Actor | null) {
  const found = yield* findBySecret(secret);
  if (found === null) return { state: "expired" } satisfies Claim;
  if (found.anonymousIds.length === 0) {
    const mine = viewer !== null && (yield* isMember(viewer, found.id));
    return (mine ? { state: "claimed", organization: { slug: found.slug } } : { state: "taken" }) satisfies Claim;
  }
  if (!live(found.expiresAt)) return { state: "expired" } satisfies Claim;
  const [servers, { projects }] = yield* Effect.all([countOrganizationMachines(found.id), readStore(found.id, { query: "projects" })]);
  return {
    state: "pending",
    organization: { name: found.name, slug: found.slug },
    servers,
    projects: projects.length,
    expiresAt: found.expiresAt,
  } satisfies Claim;
});

/**
 * The person keeps the Organization in one transaction: they become its sole owner, every row the anonymous agent
 * made now names them, the agent's token runs 90 more days, the anonymous user goes, their sessions act in it, and the
 * empty Organization their sign-in just made goes too. The secret keeps naming the Organization, so the owner claiming
 * again changes nothing and anyone else hears it was kept.
 */
export const claimOrganization = Effect.fn("Identity.claimOrganization")(function* (actor: Actor, secret: string) {
  // The Store makes its tables when it opens, and the cleanup reads them.
  yield* cloudStore;
  const database = yield* Database;
  return yield* database.transaction(Effect.gen(function* () {
    const { drizzle } = yield* Database;
    const [locked] = yield* drizzle
      .select({ id: organization.id })
      .from(organization)
      .where(eq(organization.claimSecretHash, hashClaimSecret(secret)))
      .for("update");
    if (locked === undefined) return yield* unknown();
    yield* drizzle.select({ userId: member.userId }).from(member)
      .where(eq(member.organizationId, locked.id)).orderBy(asc(member.userId)).for("update");
    const found = yield* findBySecret(secret);
    if (found === null) return yield* unknown();
    if (found.anonymousIds.length === 0) {
      if (yield* isMember(actor, found.id)) return { slug: found.slug };
      return yield* taken();
    }
    if (!live(found.expiresAt)) return yield* new Conflict({ message: "This sandbox expired.", userFacing: true });
    const { id, anonymousIds } = found;
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
    yield* drizzle.delete(user).where(inArray(user.id, anonymousIds));
    yield* updateActorSessionsOrganization(actor, id, found.slug);
    yield* removeSignInOrganization(actor, found);
    return { slug: found.slug };
  }));
});

/**
 * The Organization sign-in made for a person who signed up to claim this sandbox, if nothing ever happened in it:
 * their account is newer than the sandbox, it is their only other Organization, it carries its auto-created name, they
 * are its only member, and it has no Pairing, token, invite, billing, GitHub installation or Config Store rows.
 */
const removeSignInOrganization = Effect.fn("Identity.removeSignInOrganization")(function* (actor: Actor, sandbox: { id: string; createdAt: Date }) {
  const { drizzle } = yield* Database;
  const [person] = yield* drizzle.select({ id: user.id, name: user.name, email: user.email, createdAt: user.createdAt }).from(user).where(eq(user.id, actor.userId));
  if (person === undefined || person.createdAt <= sandbox.createdAt) return;
  const others = yield* drizzle
    .select({ id: organization.id, name: organization.name })
    .from(member)
    .innerJoin(organization, eq(organization.id, member.organizationId))
    .where(and(eq(member.userId, actor.userId), ne(organization.id, sandbox.id)))
    .for("update", { of: organization });
  const [only] = others;
  if (others.length !== 1 || only === undefined || only.name !== personalOrganizationName(person)) return;
  if (!(yield* isUntouched(only.id))) return;
  yield* drizzle.delete(organization).where(eq(organization.id, only.id));
});

/** The Config Store tables an Organization's removal clears, so no row in them means the Store holds nothing for it. */
const STORE_TABLES = ["config_project", "config_create", "config_branch", "config_check_suite", "config_build_order", "config_pull_request", "config_sync_receipt"];

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
    rowsIn(organizationBillingState, organizationBillingState.organizationId),
    rowsIn(organizationGithubInstallation, organizationGithubInstallation.organizationId),
  ]);
  if (members !== 1 || others.some((rows) => rows !== 0)) return false;
  const stored = sql.join(STORE_TABLES.map((table) => sql`select 1 from ${sql.identifier(table)} where organization_id = ${organizationId}`), sql` union all `);
  const [row] = yield* drizzle.execute<{ stored: boolean }>(sql`select exists (${stored}) as stored`, "objects");
  return row?.stored === false;
});
