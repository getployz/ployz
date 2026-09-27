import "@tanstack/react-start/server-only";
import { and, eq, exists } from "drizzle-orm";
import { Effect } from "effect";
import { member, session } from "#/modules/identity/tables";
import { organization } from "#/modules/organization/tables";
import { environment, project } from "#/modules/project/tables";
import type { Actor } from "#/modules/identity/actor";
import { Database } from "#/server/database.server";
import { Conflict } from "#/server/public-error";
import { emptyEnvironmentIntent } from "./saved-intent";
import { allocateUnique, getSlugWithSuffix } from "#/utils/slug";
import {
  projectBaseSlug,
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

/**
 * Holds a Project's row for the transaction and returns its Default Environment as it is now. Choosing the Default
 * Environment and admitting a teardown in the Project both take it, so neither acts on what the other is changing.
 */
export const lockProjectDefault = Effect.fn("EnvironmentDesign.lockProjectDefault")(function* (projectId: string) {
  const { drizzle } = yield* Database;
  const [row] = yield* drizzle.select({ defaultEnvironmentId: project.defaultEnvironmentId }).from(project)
    .where(eq(project.id, projectId)).for("update");
  return row?.defaultEnvironmentId ?? null;
});

/** Null when the Environment belongs to another project: the FK alone doesn't enforce it. */
export const setDefaultEnvironment = Effect.fn(
  "EnvironmentDesign.setDefaultEnvironment",
)(function* (projectId: string, environmentId: string) {
  const database = yield* Database;
  const rows = yield* database.drizzle
    .update(project)
    .set({ defaultEnvironmentId: environmentId })
    .where(and(
      eq(project.id, projectId),
      exists(database.drizzle.select({ id: environment.id }).from(environment)
        .where(and(eq(environment.id, environmentId), eq(environment.projectId, projectId)))),
    ))
    .returning();
  return rows[0] ?? null;
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
