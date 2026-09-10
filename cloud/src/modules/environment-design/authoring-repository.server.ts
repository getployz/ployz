import "@tanstack/react-start/server-only";
import { randomUUID } from "node:crypto";
import { and, eq } from "drizzle-orm";
import { Effect } from "effect";
import {
  resourceLineage,
  serviceLineage,
  variableGroupLineage,
} from "#/modules/environment-design/tables";
import { environment, project } from "#/modules/project/tables";
import { organizationIdForProject } from "#/db/scope-values.server";
import type { Actor } from "#/modules/identity/actor";
import { Database } from "#/server/database.server";
import { NotFound } from "#/server/public-error";
import type { EnvironmentNodeNameIdentity } from "./environment-node-names";
import { loadEnvironmentDocument } from "./working-state-repository.server";
import {
  getEnvironmentForProjectByNamespace,
  getOrganizationForUserBySlug,
  getProjectContextForActor,
} from "./workspace-repository.server";

export const getEnvironmentContextForActor = Effect.fn(
  "EnvironmentDesign.getEnvironmentContextForActor",
)(function* (
  actor: Actor,
  input: {
    readonly organizationSlug: string;
    readonly projectSlug: string;
    readonly environmentSlug: string;
  },
) {
  const projectContext = yield* getProjectContextForActor(actor, input);
  if (projectContext === null) return null;
  const environmentRecord = yield* getEnvironmentForProjectByNamespace(
    projectContext.project.id,
    input.environmentSlug,
  );
  if (environmentRecord === null) return null;
  return { ...projectContext, environment: environmentRecord };
});

export const getEnvironmentContextForActorById = Effect.fn(
  "EnvironmentDesign.getEnvironmentContextForActorById",
)(function* (
  actor: Actor,
  input: { readonly organizationSlug: string; readonly environmentId: string },
) {
  const organization = yield* getOrganizationForUserBySlug(
    actor.userId,
    input.organizationSlug,
  );
  if (organization === null) return null;
  const database = yield* Database;
  const rows = yield* database.drizzle
    .select({ project, environment })
    .from(environment)
    .innerJoin(project, eq(environment.projectId, project.id))
    .where(
      and(
        eq(environment.id, input.environmentId),
        eq(project.organizationId, organization.id),
      ),
    )
    .limit(1);
  const record = rows[0];
  return record === undefined
    ? null
    : { organization, project: record.project, environment: record.environment };
});

export const requireOrganizationForActor = Effect.fn(
  "EnvironmentDesign.requireOrganization",
)(function* (actor: Actor, organizationSlug: string) {
  const organization = yield* getOrganizationForUserBySlug(
    actor.userId,
    organizationSlug,
  );
  if (organization === null) {
    return yield* new NotFound({ message: "Organization not found." });
  }
  return organization;
});

export const requireEnvironmentForActor = Effect.fn(
  "EnvironmentDesign.requireEnvironment",
)(function* (
  actor: Actor,
  input: {
    readonly organizationSlug: string;
    readonly projectSlug: string;
    readonly environmentSlug: string;
  },
) {
  const context = yield* getEnvironmentContextForActor(actor, input);
  if (context === null) {
    return yield* new NotFound({ message: "Environment not found." });
  }
  return context;
});

export const requireEnvironmentForActorById = Effect.fn(
  "EnvironmentDesign.requireEnvironmentById",
)(function* (
  actor: Actor,
  input: { readonly organizationSlug: string; readonly environmentId: string },
) {
  const context = yield* getEnvironmentContextForActorById(actor, input);
  if (context === null) {
    return yield* new NotFound({ message: "Environment not found." });
  }
  return context;
});

export const listEnvironmentNodeNameIdentities = Effect.fn(
  "EnvironmentDesign.listEnvironmentNodeNameIdentities",
)(function* (environmentId: string) {
  const { intent } = yield* loadEnvironmentDocument(environmentId);
  return [
    ...intent.services.map((node) => ({ type: "service" as const, id: node.id, name: node.config.name })),
    ...intent.variableGroups.map((node) => ({ type: "variable_group" as const, id: node.resourceId, name: node.name })),
    ...intent.volumes.map((node) => ({ type: "volume" as const, id: node.resourceId, name: node.name })),
  ] satisfies EnvironmentNodeNameIdentity[];
});

function lineageCanonicalSlug(input: {
  readonly baseSlug: string;
  readonly lineageId: string;
}) {
  return `${input.baseSlug}-${input.lineageId.slice(0, 8)}`;
}

export const createServiceLineage = Effect.fn(
  "EnvironmentDesign.createServiceLineage",
)(function* (input: {
  readonly projectId: string;
  readonly name: string;
  readonly slug: string;
}) {
  const database = yield* Database;
  const id = randomUUID();
  const rows = yield* database.drizzle
    .insert(serviceLineage)
    .values({
      id,
      projectId: input.projectId,
      canonicalName: input.name,
      canonicalSlug: lineageCanonicalSlug({ baseSlug: input.slug, lineageId: id }),
    })
    .onConflictDoNothing()
    .returning({ id: serviceLineage.id });
  return rows[0] ?? null;
});

export const createVariableGroupLineage = Effect.fn(
  "EnvironmentDesign.createVariableGroupLineage",
)(function* (input: {
  readonly projectId: string;
  readonly name: string;
  readonly slug: string;
}) {
  const database = yield* Database;
  const id = randomUUID();
  const rows = yield* database.drizzle
    .insert(variableGroupLineage)
    .values({
      id,
      projectId: input.projectId,
      canonicalName: input.name,
      canonicalSlug: lineageCanonicalSlug({ baseSlug: input.slug, lineageId: id }),
    })
    .onConflictDoNothing()
    .returning({ id: variableGroupLineage.id });
  return rows[0] ?? null;
});

export const createResourceLineage = Effect.fn(
  "EnvironmentDesign.createResourceLineage",
)(function* (input: {
  readonly projectId: string;
  readonly name: string;
  readonly slug: string;
}) {
  const database = yield* Database;
  const id = randomUUID();
  const rows = yield* database.drizzle
    .insert(resourceLineage)
    .values({
      id,
      organizationId: organizationIdForProject(input.projectId),
      projectId: input.projectId,
      canonicalName: input.name,
      canonicalSlug: lineageCanonicalSlug({ baseSlug: input.slug, lineageId: id }),
    })
    .onConflictDoNothing()
    .returning();
  return rows[0] ?? null;
});
