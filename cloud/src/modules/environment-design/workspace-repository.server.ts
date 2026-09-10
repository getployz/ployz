import "@tanstack/react-start/server-only";
import { and, asc, eq, inArray } from "drizzle-orm";
import { Effect } from "effect";
import { member, session } from "#/modules/identity/tables";
import { organization } from "#/modules/organization/tables";
import { environment, project, userProjectPreference } from "#/modules/project/tables";
import { organizationIdForProject } from "#/db/scope-values.server";
import type { Actor } from "#/modules/identity/actor";
import { Database } from "#/server/database.server";
import { Conflict } from "#/server/public-error";
import { emptyEnvironmentIntent } from "./saved-intent";
import { allocateUnique, getSlugWithSuffix } from "#/utils/slug";
import {
  projectBaseSlug,
  type Environment,
  personalOrganizationBaseSlug,
  personalOrganizationName,
  type PersonalOrganizationUser,
} from "./workspace-schemas";
export type { PersonalOrganizationUser } from "./workspace-schemas";

const organizationColumns = {
  id: organization.id,
  name: organization.name,
  slug: organization.slug,
  logo: organization.logo,
};

const projectColumns = {
  id: project.id,
  organizationId: project.organizationId,
  name: project.name,
  slug: project.slug,
};

const environmentColumns = {
  id: environment.id,
  projectId: environment.projectId,
  organizationId: environment.organizationId,
  name: environment.name,
  namespace: environment.namespace,
};

const preferenceColumns = {
  id: userProjectPreference.id,
  userId: userProjectPreference.userId,
  projectId: userProjectPreference.projectId,
  environmentId: userProjectPreference.environmentId,
};

export const getOrganizationSlugById = Effect.fn(
  "EnvironmentDesign.getOrganizationSlugById",
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
  "EnvironmentDesign.listOrganizationIds",
)(function* () {
  const database = yield* Database;
  const rows = yield* database.drizzle
    .select({ id: organization.id })
    .from(organization);
  return rows.map((row) => row.id);
});

export const getOrganizationForUserBySlug = Effect.fn(
  "EnvironmentDesign.getOrganizationForUserBySlug",
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
  "EnvironmentDesign.getFirstOrganizationIdForUser",
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
  "EnvironmentDesign.createPersonalOrganization",
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
  "EnvironmentDesign.ensurePersonalOrganizationForUser",
)(function* (user: PersonalOrganizationUser) {
  const database = yield* Database;
  return yield* database.transaction(
    Effect.gen(function* () {
      const existing = yield* getFirstOrganizationIdForUser(user.id);
      if (existing !== null) return existing;
      const organizationId = yield* createPersonalOrganization(user);
      yield* (yield* Database).drizzle
        .insert(member)
        .values({ userId: user.id, organizationId, role: "owner" })
        .onConflictDoNothing();
      return organizationId;
    }),
  );
});

export const listOrganizationsForActor = Effect.fn(
  "EnvironmentDesign.listOrganizationsForActor",
)(function* (actor: Actor) {
  const database = yield* Database;
  return yield* database.drizzle
    .select(organizationColumns)
    .from(member)
    .innerJoin(organization, eq(member.organizationId, organization.id))
    .where(eq(member.userId, actor.userId));
});

export const updateActorSessionsOrganization = Effect.fn(
  "EnvironmentDesign.updateActorSessionsOrganization",
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

export const getProjectForOrganizationBySlug = Effect.fn(
  "EnvironmentDesign.getProjectForOrganizationBySlug",
)(function* (organizationId: string, slug: string) {
  const database = yield* Database;
  const rows = yield* database.drizzle
    .select(projectColumns)
    .from(project)
    .where(and(eq(project.organizationId, organizationId), eq(project.slug, slug)))
    .limit(1);
  return rows[0] ?? null;
});

export const listProjectsForOrganization = Effect.fn(
  "EnvironmentDesign.listProjectsForOrganization",
)(function* (organizationId: string) {
  const database = yield* Database;
  return yield* database.drizzle
    .select(projectColumns)
    .from(project)
    .where(eq(project.organizationId, organizationId));
});

export const createProject = Effect.fn("EnvironmentDesign.createProject")(
  function* (input: { readonly organizationId: string; readonly name: string }) {
    const database = yield* Database;
    const name = input.name.trim();
    const baseSlug = projectBaseSlug(name);
    return yield* allocateUnique({
      tryAttempt: (attempt) =>
        Effect.gen(function* () {
          const rows = yield* database.drizzle
            .insert(project)
            .values({
              organizationId: input.organizationId,
              name,
              slug: getSlugWithSuffix(baseSlug, attempt),
            })
            .onConflictDoNothing()
            .returning();
          return rows[0] ?? null;
        }),
      exhausted: new Conflict({
        message: `Could not allocate a project slug in ${input.organizationId}.`,
      }),
    });
  },
);

export const createEnvironmentRecord = Effect.fn(
  "EnvironmentDesign.createEnvironmentRecord",
)(function* (input: {
  readonly projectId: string;
  readonly organizationId: string;
  readonly name: string;
  readonly namespace: string;
}) {
  const database = yield* Database;
  const rows = yield* database.drizzle
    .insert(environment)
    .values({ ...input, intent: emptyEnvironmentIntent(input.namespace) })
    .returning();
  const created = rows[0];
  if (created === undefined) {
    return yield* Effect.die("PostgreSQL did not return the created environment.");
  }
  return created;
});

export const getEnvironmentForProjectByNamespace = Effect.fn(
  "EnvironmentDesign.getEnvironmentForProjectByNamespace",
)(function* (projectId: string, namespace: string) {
  const database = yield* Database;
  const rows = yield* database.drizzle
    .select(environmentColumns)
    .from(environment)
    .where(
      and(
        eq(environment.projectId, projectId),
        eq(environment.namespace, namespace),
      ),
    )
    .limit(1);
  return rows[0] ?? null;
});

export const listEnvironmentsForProject = Effect.fn(
  "EnvironmentDesign.listEnvironmentsForProject",
)(function* (projectId: string) {
  const database = yield* Database;
  return yield* database.drizzle
    .select(environmentColumns)
    .from(environment)
    .where(eq(environment.projectId, projectId))
    .orderBy(asc(environment.createdAt));
});

export const listEnvironmentsForProjects = Effect.fn(
  "EnvironmentDesign.listEnvironmentsForProjects",
)(function* (projectIds: readonly string[]) {
  if (projectIds.length === 0) return [] satisfies Environment[];
  const database = yield* Database;
  return yield* database.drizzle
    .select(environmentColumns)
    .from(environment)
    .where(inArray(environment.projectId, [...projectIds]))
    .orderBy(asc(environment.projectId), asc(environment.createdAt));
});

export const getEnvironmentByIdForProject = Effect.fn(
  "EnvironmentDesign.getEnvironmentByIdForProject",
)(function* (projectId: string, environmentId: string) {
  const database = yield* Database;
  const rows = yield* database.drizzle
    .select(environmentColumns)
    .from(environment)
    .where(and(eq(environment.projectId, projectId), eq(environment.id, environmentId)))
    .limit(1);
  return rows[0] ?? null;
});

export const getPreferenceForUserAndProject = Effect.fn(
  "EnvironmentDesign.getPreferenceForUserAndProject",
)(function* (userId: string, projectId: string) {
  const database = yield* Database;
  const rows = yield* database.drizzle
    .select(preferenceColumns)
    .from(userProjectPreference)
    .where(
      and(
        eq(userProjectPreference.userId, userId),
        eq(userProjectPreference.projectId, projectId),
      ),
    )
    .limit(1);
  return rows[0] ?? null;
});

export const listPreferencesForUserAndProjects = Effect.fn(
  "EnvironmentDesign.listPreferencesForUserAndProjects",
)(function* (userId: string, projectIds: readonly string[]) {
  if (projectIds.length === 0) return [];
  const database = yield* Database;
  return yield* database.drizzle
    .select(preferenceColumns)
    .from(userProjectPreference)
    .where(
      and(
        eq(userProjectPreference.userId, userId),
        inArray(userProjectPreference.projectId, [...projectIds]),
      ),
    );
});

export const upsertUserProjectPreference = Effect.fn(
  "EnvironmentDesign.upsertUserProjectPreference",
)(function* (input: {
  readonly userId: string;
  readonly projectId: string;
  readonly environmentId: string;
}) {
  const database = yield* Database;
  const rows = yield* database.drizzle
    .insert(userProjectPreference)
    .values({
      organizationId: organizationIdForProject(input.projectId),
      userId: input.userId,
      projectId: input.projectId,
      environmentId: input.environmentId,
    })
    .onConflictDoUpdate({
      target: [userProjectPreference.userId, userProjectPreference.projectId],
      set: { environmentId: input.environmentId, updatedAt: new Date() },
    })
    .returning(preferenceColumns);
  const preference = rows[0];
  if (preference === undefined) {
    return yield* Effect.die("PostgreSQL did not return the project preference.");
  }
  return preference;
});

export const getProjectContextForActor = Effect.fn(
  "EnvironmentDesign.getProjectContextForActor",
)(function* (actor: Actor, input: { readonly organizationSlug: string; readonly projectSlug: string }) {
  const organizationRecord = yield* getOrganizationForUserBySlug(
    actor.userId,
    input.organizationSlug,
  );
  if (organizationRecord === null) return null;
  const projectRecord = yield* getProjectForOrganizationBySlug(
    organizationRecord.id,
    input.projectSlug,
  );
  if (projectRecord === null) return null;
  return { organization: organizationRecord, project: projectRecord };
});
