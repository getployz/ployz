import "@tanstack/react-start/server-only";
import { and, eq } from "drizzle-orm";
import { Data, Effect } from "effect";
import type { Actor } from "#/modules/identity/actor";
import { member, session } from "#/modules/identity/tables";
import { Polar } from "#/modules/billing/polar-provider.server";
import { Database } from "#/server/database.server";
import { Conflict, NotFound } from "#/server/public-error";
import { allocateUnique, getSlugWithSuffix } from "#/utils/slug";
import {
  personalOrganizationBaseSlug,
  personalOrganizationName,
  type PersonalOrganizationUser,
  type SyncOrganizationSlug,
} from "./organization-state";
import { organization } from "./tables";

const organizationColumns = {
  id: organization.id,
  name: organization.name,
  slug: organization.slug,
  logo: organization.logo,
};

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

export const getOrganizationForUserBySlug = Effect.fn(
  "Organization.getOrganizationForUserBySlug",
)(function* (userId: string, slug: string) {
  const database = yield* Database;
  const rows = yield* database.drizzle
    .select({ id: organization.id, slug: organization.slug })
    .from(member)
    .innerJoin(organization, eq(member.organizationId, organization.id))
    .where(and(eq(member.userId, userId), eq(organization.slug, slug)))
    .limit(1);
  return rows[0] ?? null;
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

const createPersonalOrganization = Effect.fn(
  "Organization.createPersonalOrganization",
)(function* (user: PersonalOrganizationUser) {
  const database = yield* Database;
  const name = personalOrganizationName(user);
  const baseSlug = personalOrganizationBaseSlug(user);
  return yield* allocateUnique({
    tryAttempt: (attempt) =>
      Effect.gen(function* () {
        const rows = yield* database.drizzle
          .insert(organization)
          .values({ name, slug: getSlugWithSuffix(baseSlug, attempt) })
          .onConflictDoNothing()
          .returning({ id: organization.id });
        return rows[0]?.id ?? null;
      }),
    exhausted: new Conflict({
      message: `Could not allocate an organization slug for ${user.id}.`,
    }),
  });
});

export const ensurePersonalOrganizationForUser = Effect.fn(
  "Organization.ensurePersonalOrganizationForUser",
)(function* (user: PersonalOrganizationUser) {
  const database = yield* Database;
  return yield* database.transaction(
    Effect.gen(function* () {
      const existing = yield* getFirstOrganizationIdForUser(user.id);
      if (existing !== null) return existing;
      const organizationId = yield* createPersonalOrganization(user);
      yield* database.drizzle
        .insert(member)
        .values({ userId: user.id, organizationId, role: "owner" })
        .onConflictDoNothing();
      return organizationId;
    }),
  );
});

export const listOrganizationsForActor = Effect.fn(
  "Organization.listOrganizationsForActor",
)(function* (actor: Actor) {
  const database = yield* Database;
  return yield* database.drizzle
    .select(organizationColumns)
    .from(member)
    .innerJoin(organization, eq(member.organizationId, organization.id))
    .where(eq(member.userId, actor.userId));
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

/** The actor's Organization named `slug`; NotFound when they aren't a member of it. */
export const requireOrganizationForMember = Effect.fn("Organization.requireForMember")(function* (actor: Actor, slug: string) {
  const found = yield* getOrganizationForUserBySlug(actor.userId, slug);
  if (found === null) return yield* new NotFound({ message: "Organization not found." });
  return found;
});

export const getOrganizationState = Effect.fn(
  "Organization.getOrganizationState",
)(function* (actor: Actor, organizationSlug?: string) {
  const organizations = yield* listOrganizationsForActor(actor);
  const polar = yield* Polar;
  return {
    billingEnabled: polar.mode === "hosted",
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
  const organization = yield* requireOrganizationForMember(actor, input.organizationSlug);
  yield* updateActorSessionsOrganization(
    actor,
    organization.id,
    input.organizationSlug,
  );
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
