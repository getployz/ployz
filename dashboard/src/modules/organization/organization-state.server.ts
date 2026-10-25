import "@tanstack/react-start/server-only";
import { and, eq } from "drizzle-orm";
import { Data, Effect } from "effect";
import type { Actor, SessionActor } from "#/modules/identity/actor";
import { takeDeviceOrganization } from "#/modules/identity/device-organization.server";
import { invitation, member, session, user } from "#/modules/identity/tables";
import { Polar } from "#/modules/billing/polar-provider.server";
import { listAccessibleOrganizations, requireAccess, SupportStaff } from "#/modules/organization/access.server";
import { AppConfig } from "#/server/config.server";
import { Database } from "#/server/database.server";
import { Conflict, Forbidden, NotFound, Unauthorized } from "#/server/public-error";
import { allocateUnique, getSlugWithSuffix } from "#/utils/slug";
import {
  type CreateOrganization,
  personalOrganizationBaseSlug,
  personalOrganizationName,
  type PersonalOrganizationUser,
  type RenameOrganization,
  type SyncOrganizationSlug,
} from "./organization-state";
import { organization } from "./tables";

export const getOrganizationSlugById = Effect.fn(
  "Organization.getOrganizationSlugById",
)(function* (organizationId: string) {
  const database = yield* Database;
  const rows = yield* database.drizzle
    .select({ slug: organization.slug })
    .from(organization)
    .where(eq(organization.id, organizationId))
    .limit(1);
  return rows[0]?.slug ?? null;
});

export const listOrganizationIds = Effect.fn(
  "Organization.listOrganizationIds",
)(function* () {
  const database = yield* Database;
  const rows = yield* database.drizzle
    .select({ id: organization.id })
    .from(organization);
  return rows.map((row) => row.id);
});

export const getFirstOrganizationIdForUser = Effect.fn(
  "Organization.getFirstOrganizationIdForUser",
)(function* (userId: string) {
  const database = yield* Database;
  const rows = yield* database.drizzle
    .select({ organizationId: member.organizationId })
    .from(member)
    .where(eq(member.userId, userId))
    .limit(1);
  return rows[0]?.organizationId ?? null;
});

/** `/cloud/<slug>` paths Cloud's own pages take. A base slug among them always gets a suffix. */
const RESERVED_SLUGS: ReadonlySet<string> = new Set(["account"]);

/** Inserts an Organization under the first free slug from `baseSlug`, owned by `userId`. */
const insertOrganization = Effect.fn("Organization.insertOrganization")(function* (
  userId: string,
  name: string,
  baseSlug: string,
) {
  const database = yield* Database;
  const created = yield* allocateUnique({
    tryAttempt: (attempt) =>
      Effect.gen(function* () {
        const rows = yield* database.drizzle
          .insert(organization)
          .values({ name, slug: getSlugWithSuffix(baseSlug, RESERVED_SLUGS.has(baseSlug) ? attempt + 1 : attempt) })
          .onConflictDoNothing()
          .returning({ id: organization.id, slug: organization.slug });
        return rows[0] ?? null;
      }),
    exhausted: new Conflict({
      message: `Could not allocate an organization slug for ${userId}.`,
    }),
  });
  yield* database.drizzle
    .insert(member)
    .values({ userId, organizationId: created.id, role: "owner" })
    .onConflictDoNothing();
  return created;
});

export const ensurePersonalOrganizationForUser = Effect.fn(
  "Organization.ensurePersonalOrganizationForUser",
)(function* (user: PersonalOrganizationUser) {
  const database = yield* Database;
  return yield* database.transaction(
    Effect.gen(function* () {
      const existing = yield* getFirstOrganizationIdForUser(user.id);
      if (existing !== null) return existing;
      const created = yield* insertOrganization(
        user.id,
        personalOrganizationName(user),
        personalOrganizationBaseSlug(user),
      );
      return created.id;
    }),
  );
});

/** A new Organization the actor owns; their session then acts in it. Returns its slug. */
export const createOrganization = Effect.fn("Organization.create")(function* (actor: SessionActor, input: CreateOrganization) {
  const database = yield* Database;
  const created = yield* database.transaction(insertOrganization(actor.userId, input.name, input.slug));
  yield* openOrganizationInSession(actor, created.id, created.slug);
  return created.slug;
});

/** A new name, by an owner or admin, never by a support visit. The slug, and so every address and URL, stays. */
export const renameOrganization = Effect.fn("Organization.rename")(function* (actor: Actor, input: RenameOrganization) {
  const access = yield* requireAccess(actor, input.organizationSlug, "org:settings");
  if (access.kind === "support") return yield* new Forbidden({ message: "Only an owner or admin of this Organization can do this." });
  const { id } = access.organization;
  const { drizzle } = yield* Database;
  yield* drizzle.update(organization).set({ name: input.name }).where(eq(organization.id, id));
  return { name: input.name };
});

/**
 * Makes the Organization the one the actor's own session acts in. Their other sessions, another browser or a `ployz`
 * device, keep theirs: each chose its own.
 */
export const openOrganizationInSession = Effect.fn(
  "Organization.openOrganizationInSession",
)(function* (actor: SessionActor, organizationId: string, organizationSlug: string) {
  const database = yield* Database;
  yield* database.drizzle
    .update(session)
    .set({
      activeOrganizationId: organizationId,
      activeOrganizationSlug: organizationSlug,
      updatedAt: new Date(),
    })
    .where(and(eq(session.id, actor.credential.id), eq(session.userId, actor.userId)));
});

/** Someone who once joined by invite already knows the Organization exists, so its name tells them nothing new. */
const formerOrganizationName = Effect.fn("Organization.formerOrganizationName")(function* (userId: string, organizationSlug: string) {
  const { drizzle } = yield* Database;
  const [row] = yield* drizzle.select({ name: organization.name }).from(invitation)
    .innerJoin(organization, eq(organization.id, invitation.organizationId))
    .where(and(eq(invitation.acceptedByUserId, userId), eq(organization.slug, organizationSlug)))
    .limit(1);
  return row?.name ?? null;
});

export const getOrganizationState = Effect.fn(
  "Organization.getOrganizationState",
)(function* (actor: Actor, organizationSlug?: string) {
  const organizations = yield* listAccessibleOrganizations(actor);
  const polar = yield* Polar;
  const staff = yield* SupportStaff;
  const config = yield* AppConfig;
  const accessible = organizationSlug !== undefined && organizations.some((organization) => organization.slug === organizationSlug);
  const activeOrganization = organizations.find((organization) => organization.slug === organizationSlug) ?? organizations[0] ?? null;
  return {
    formerOrganization: organizationSlug === undefined || accessible ? null : yield* formerOrganizationName(actor.userId, organizationSlug),
    billingEnabled: polar.mode === "hosted",
    uptimeEnabled: config.uptimeEnabled,
    /** Whether this Cloud has Ployz support at all, so the support switch means something. */
    supportAvailable: staff.available,
    activeOrganization,
    organizations,
  };
});

export const syncOrganizationSlug = Effect.fn(
  "Organization.syncOrganizationSlug",
)(function* (actor: SessionActor, input: SyncOrganizationSlug) {
  const { kind, organization } = yield* requireAccess(actor, input.organizationSlug, "org:read");
  // Support visits; the session stays in the staff member's own Organization.
  if (kind === "member") {
    yield* openOrganizationInSession(actor, organization.id, input.organizationSlug);
  }
  return {
    organizationId: organization.id,
    organizationSlug: input.organizationSlug,
  };
});

type SessionRecord = {
  id: string;
  userId: string;
  token?: string | null;
  activeOrganizationId?: string | null;
  activeOrganizationSlug?: string | null;
};

type SessionContext = {
  context: {
    internalAdapter: {
      findUserById(userId: string): Promise<PersonalOrganizationUser | null>;
      updateSession(
        token: string,
        session: {
          activeOrganizationId: string;
          activeOrganizationSlug: string | null;
        },
      ): Promise<SessionRecord | null>;
    };
  };
};

export class WorkspaceAuthHookFailure extends Data.TaggedError(
  "WorkspaceAuthHookFailure",
)<{ readonly cause: unknown }> {}

const setActiveOrganizationForSession = Effect.fn(
  "Organization.setActiveOrganizationForSession",
)(function* (
  sessionRecord: SessionRecord,
  organizationId: string,
  ctx: SessionContext,
) {
  const organizationSlug = yield* getOrganizationSlugById(organizationId);

  const token = sessionRecord.token;
  if (token) {
    yield* Effect.tryPromise({
      try: () => ctx.context.internalAdapter.updateSession(token, {
        activeOrganizationId: organizationId,
        activeOrganizationSlug: organizationSlug,
      }),
      catch: (cause) => new WorkspaceAuthHookFailure({ cause }),
    });
    return;
  }

  const database = yield* Database;
  yield* database.drizzle
    .update(session)
    .set({
      activeOrganizationId: organizationId,
      activeOrganizationSlug: organizationSlug,
    })
    .where(eq(session.id, sessionRecord.id));
});

export const handleUserCreated = Effect.fn("Organization.handleUserCreated")(
  function* (user: PersonalOrganizationUser) {
    return yield* ensurePersonalOrganizationForUser(user);
  },
);

/**
 * A new session acts in an Organization from the start. A `ployz login` session redeeming `deviceCode` takes the one
 * the `/device` page named when it was approved; any other takes the user's first, made if they have none.
 */
export const handleSessionCreated = Effect.fn("Organization.handleSessionCreated")(
function* (
  sessionRecord: SessionRecord,
  ctx: SessionContext | null,
  deviceCode: string | null,
) {
  if (!ctx || sessionRecord.activeOrganizationId) return;
  const approved = deviceCode === null ? null : yield* takeDeviceOrganization(deviceCode, sessionRecord.userId);
  let organizationId = approved ?? (yield* getFirstOrganizationIdForUser(sessionRecord.userId));
  if (!organizationId) {
    const user = yield* Effect.tryPromise({
      try: () => ctx.context.internalAdapter.findUserById(sessionRecord.userId),
      catch: (cause) => new WorkspaceAuthHookFailure({ cause }),
    });
    if (!user) return;
    organizationId = yield* ensurePersonalOrganizationForUser(user);
  }
  if (!organizationId) return;
  yield* setActiveOrganizationForSession(sessionRecord, organizationId, ctx);
});

/**
 * For an actor whose session acts in no Organization (theirs was deleted): their first Organization, else a new
 * personal one; their session then acts in it. Returns its slug.
 */
export const openOrCreateOrganization = Effect.fn("Organization.openOrCreate")(function* (actor: SessionActor) {
  const database = yield* Database;
  const [row] = yield* database.drizzle.select({ id: user.id, name: user.name, email: user.email }).from(user)
    .where(eq(user.id, actor.userId)).limit(1);
  if (!row) return yield* new NotFound({ message: "User not found." });
  const organizationId = yield* ensurePersonalOrganizationForUser(row);
  const slug = yield* getOrganizationSlugById(organizationId);
  if (!slug) return yield* new NotFound({ message: "Organization not found." });
  yield* openOrganizationInSession(actor, organizationId, slug);
  return slug;
});

/** A new Organization the user owns. Only session `sessionId` moves into it; the user's other sessions stay put. */
export const createOrganizationForSession = Effect.fn("Organization.createForSession")(function* (
  userId: string,
  sessionId: string,
  input: CreateOrganization,
) {
  const database = yield* Database;
  return yield* database.transaction(Effect.gen(function* () {
    const created = yield* insertOrganization(userId, input.name, input.slug);
    const { drizzle: tx } = yield* Database;
    const moved = yield* tx
      .update(session)
      .set({ activeOrganizationId: created.id, activeOrganizationSlug: created.slug, updatedAt: new Date() })
      .where(and(eq(session.id, sessionId), eq(session.userId, userId)))
      .returning({ id: session.id });
    // The session ended mid-request: roll the Organization back with it.
    if (moved.length === 0) return yield* new Unauthorized();
    return { id: created.id, slug: created.slug, name: input.name };
  }));
});
