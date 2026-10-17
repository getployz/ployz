import "@tanstack/react-start/server-only";
import crypto from "node:crypto";
import { and, asc, eq, gte, inArray, ne, sql } from "drizzle-orm";
import { Effect } from "effect";
import { readStore } from "#/modules/config-store/config-store.server";
import { cloudStore } from "#/modules/config-store/store-sdk.server";
import type { Actor } from "#/modules/identity/actor";
import { claimSecretFor, hashClaimSecret } from "#/modules/identity/claim-required.server";
import type { ClaimSignIn } from "#/modules/identity/claim-sign-in.server";
import { member, organizationToken, user } from "#/modules/identity/tables";
import { countOrganizationMachines } from "#/modules/machines/connections.server";
import { machineEnrollmentToken, machineRemoveAttempt, serverAccess } from "#/modules/machines/tables";
import { fleetToken, serverEnrollment } from "#/modules/machines/waiting/tables";
import { updateActorSessionsOrganization } from "#/modules/organization/organization-state.server";
import { organization } from "#/modules/organization/tables";
import { Database } from "#/server/database.server";
import { Conflict, NotFound } from "#/server/public-error";

/** `ployz login` tokens default to 90 days; the claimed agent's token gets the same. */
const CLAIMED_TOKEN_TTL_MS = 90 * 24 * 60 * 60 * 1000;
const HOUR_MS = 60 * 60 * 1000;

/** What the claim page shows for a secret, to a viewer who may be signed in. */
export type Claim =
  | {
    readonly state: "pending";
    readonly organization: { readonly name: string; readonly slug: string };
    readonly servers: number;
    readonly projects: number;
    /** Whole hours until teardown, counted on the server so the first paint already says it. */
    readonly hoursLeft: number;
  }
  | { readonly state: "claimed"; readonly organization: { readonly slug: string } }
  | { readonly state: "taken" }
  | { readonly state: "expired" };

const unknown = () => new NotFound({ message: "This claim link is unknown." });
const taken = () => new Conflict({ message: "Someone already kept this sandbox.", userFacing: true });

const sameSecret = (a: string, b: string) => crypto.timingSafeEqual(
  crypto.createHash("sha256").update(a).digest(),
  crypto.createHash("sha256").update(b).digest(),
);

/**
 * The Organization under this secret, its anonymous owners (none once a person kept it), and when their tokens lapse.
 * The secret must also be the one the current encryption secret derives, so rotating it retires every outstanding URL.
 */
const findBySecret = Effect.fn("Identity.findClaimOrganization")(function* (secret: string) {
  const { drizzle } = yield* Database;
  const [found] = yield* drizzle
    .select({ id: organization.id, name: organization.name, slug: organization.slug })
    .from(organization)
    .where(eq(organization.claimSecretHash, hashClaimSecret(secret)))
    .limit(1);
  if (found === undefined || !sameSecret(secret, yield* claimSecretFor(found.id))) return null;
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
    hoursLeft: Math.ceil((found.expiresAt.getTime() - Date.now()) / HOUR_MS),
  } satisfies Claim;
});

/**
 * The person keeps the Organization in one transaction: they become its sole owner, every row the anonymous agent
 * made now names them, the agent's token runs 90 more days, the anonymous user goes, and their sessions act in it.
 * When the claim page started their sign-in, the empty Organization that sign-in made goes too. The secret keeps
 * naming the Organization, so the owner claiming again changes nothing and anyone else hears it was kept.
 */
export const claimOrganization = Effect.fn("Identity.claimOrganization")(function* (actor: Actor, secret: string, signIn?: ClaimSignIn) {
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
    yield* drizzle.update(serverEnrollment).set({ requestedByUserId: actor.userId })
      .where(inArray(serverEnrollment.requestedByUserId, anonymousIds));
    yield* drizzle.delete(user).where(inArray(user.id, anonymousIds));
    yield* updateActorSessionsOrganization(actor, id, found.slug);
    if (signIn !== undefined) yield* removeSignInOrganization(actor, id, signIn);
    return { slug: found.slug };
  }));
});

/**
 * Back from the GitHub sign-in the claim page started: the person keeps the sandbox without a second click. A claim
 * that cannot go through (taken, expired, unknown) leaves the page to say so.
 */
export const finishClaimSignIn = Effect.fn("Identity.finishClaimSignIn")(function* (secret: string, actor: Actor, signIn: ClaimSignIn) {
  yield* claimOrganization(actor, secret, signIn).pipe(
    Effect.catchTags({ NotFound: () => Effect.void, Conflict: () => Effect.void }),
  );
  return yield* readClaim(secret, actor);
});

/**
 * The Organization the claim's own sign-in made, if nothing ever happened in it: the person's account and that
 * Organization both date from after the claim page started the sign-in, it is their only other Organization, and
 * no table that names an Organization holds a row for it beyond their membership.
 */
const removeSignInOrganization = Effect.fn("Identity.removeSignInOrganization")(function* (actor: Actor, sandboxId: string, signIn: ClaimSignIn) {
  const { drizzle } = yield* Database;
  const [person] = yield* drizzle.select({ createdAt: user.createdAt }).from(user).where(eq(user.id, actor.userId));
  if (person === undefined || person.createdAt < signIn.startedAt) return;
  const others = yield* drizzle
    .select({ id: organization.id, createdAt: organization.createdAt })
    .from(member)
    .innerJoin(organization, eq(organization.id, member.organizationId))
    .where(and(eq(member.userId, actor.userId), ne(organization.id, sandboxId)))
    .for("update", { of: organization });
  const [only] = others;
  if (others.length !== 1 || only === undefined || only.createdAt < signIn.startedAt) return;
  if (!(yield* isUntouched(only.id))) return;
  yield* drizzle.delete(organization).where(and(eq(organization.id, only.id), gte(organization.createdAt, signIn.startedAt)));
});

/**
 * One member and no row in any other table with an `organization_id` column, read from the schema so new tables count.
 * The change log is left out: its rows only record writes to the other tables, and making the Organization wrote some.
 */
const isUntouched = Effect.fn("Identity.isUntouchedOrganization")(function* (organizationId: string) {
  const { drizzle } = yield* Database;
  const tables = yield* drizzle.execute<{ name: string }>(sql`
    select c.table_name as name
    from information_schema.columns c
    join information_schema.tables t on t.table_schema = c.table_schema and t.table_name = c.table_name
    where c.table_schema = current_schema() and c.column_name = 'organization_id' and t.table_type = 'BASE TABLE'
      and c.table_name not in ('member', 'organization_change')`, "objects");
  const rows = sql.join(
    [
      sql`(select 1 from member where organization_id = ${organizationId} offset 1)`,
      ...tables.map(({ name }) => sql`(select 1 from ${sql.identifier(name)} where organization_id = ${organizationId})`),
    ],
    sql` union all `,
  );
  const [row] = yield* drizzle.execute<{ touched: boolean }>(sql`select exists (${rows}) as touched`, "objects");
  return row?.touched === false;
});
