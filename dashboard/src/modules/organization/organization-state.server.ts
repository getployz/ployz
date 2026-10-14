import "@tanstack/react-start/server-only";
import { and, eq } from "drizzle-orm";
import { Data, Effect } from "effect";
import type { Actor } from "#/modules/identity/actor";
import { invitation, member, session, user } from "#/modules/identity/tables";
import { Polar } from "#/modules/billing/polar-provider.server";
import { listAccessibleOrganizations, requireAccess, SupportStaff } from "#/modules/organization/access.server";
import { AppConfig } from "#/server/config.server";
import { Database } from "#/server/database.server";
import { Conflict, NotFound } from "#/server/public-error";
import { allocateUnique, getSlugWithSuffix } from "#/utils/slug";
import {
  type CreateOrganization,
  personalOrganizationBaseSlug,
  personalOrganizationName,
  type PersonalOrganizationUser,
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
          .values({ name, slug: getSlugWithSuffix(baseSlug, attempt) })
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

/** A new Organization the actor owns; their sessions then act in it. Returns its slug. */
export const createOrganization = Effect.fn("Organization.create")(function* (actor: Actor, input: CreateOrganization) {
  const database = yield* Database;
  const created = yield* database.transaction(insertOrganization(actor.userId, input.name, input.slug));
  yield* updateActorSessionsOrganization(actor, created.id, created.slug);
  return created.slug;
});

export const updateActorSessionsOrganization = Effect.fn(
  "Organization.updateActorSessionsOrganization",
)(function* (actor: Actor, organizationId: string, organizationSlug: string) {
  const database = yield* Database;
  yield* database.drizzle
    .update(session)
    .set({
      activeOrganizationId: organizationId,
      activeOrganizationSlug: organizationSlug,
      updatedAt: new Date(),
    })
    .where(eq(session.userId, actor.userId));
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
  return {
    formerOrganization: organizationSlug === undefined || accessible ? null : yield* formerOrganizationName(actor.userId, organizationSlug),
    billingEnabled: polar.mode === "hosted",
    uptimeEnabled: config.uptimeEnabled,
    /** Whether this Cloud has Ployz support at all, so the support switch means something. */
    supportAvailable: staff.available,
    activeOrganization:
      organizations.find((organization) => organization.slug === organizationSlug) ??
      organizations[0] ??
      null,
    organizations,
  };
});

export const syncOrganizationSlug = Effect.fn(
  "Organization.syncOrganizationSlug",
)(function* (actor: Actor, input: SyncOrganizationSlug) {
  const { kind, organization } = yield* requireAccess(actor, input.organizationSlug, "operate");
  // Support visits; its sessions stay in the staff member's own Organization.
  if (kind === "member") {
    yield* updateActorSessionsOrganization(actor, organization.id, input.organizationSlug);
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
    yield* ensurePersonalOrganizationForUser(user);
  },
);

export const handleSessionCreated = Effect.fn("Organization.handleSessionCreated")(
function* (
  sessionRecord: SessionRecord,
  ctx: SessionContext | null,
) {
  if (!ctx || sessionRecord.activeOrganizationId) return;
  let organizationId = yield* getFirstOrganizationIdForUser(sessionRecord.userId);
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
 * personal one; their sessions then act in it. Returns its slug.
 */
export const openOrCreateOrganization = Effect.fn("Organization.openOrCreate")(function* (actor: Actor) {
  const database = yield* Database;
  const [row] = yield* database.drizzle.select({ id: user.id, name: user.name, email: user.email }).from(user)
    .where(eq(user.id, actor.userId)).limit(1);
  if (!row) return yield* new NotFound({ message: "User not found." });
  const organizationId = yield* ensurePersonalOrganizationForUser(row);
  const slug = yield* getOrganizationSlugById(organizationId);
  if (!slug) return yield* new NotFound({ message: "Organization not found." });
  yield* updateActorSessionsOrganization(actor, organizationId, slug);
  return slug;
});
