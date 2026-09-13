import "@tanstack/react-start/server-only";
import { adjectives, animals, uniqueNamesGenerator } from "unique-names-generator";
import { Effect } from "effect";
import { sqlErrorFrom } from "#/server/database.server";
import { Conflict, NotFound } from "#/server/public-error";
import type { Actor } from "#/modules/identity/actor";
import { withMutationResult } from "#/server/mutation-result.server";
import {
  createCanonicalEnvironmentNamespace,
  DEFAULT_ENVIRONMENT_NAME,
  type CreateEnvironment,
  type EnvironmentBySlug,
  type EnvironmentList,
  type ProjectBySlug,
  type ProjectList,
  type SyncOrganizationSlug,
} from "./workspace-schemas";
import {
  createEnvironmentRecord,
  createProject,
  getEnvironmentForProjectByNamespace,
  getPreferenceForUserAndProject,
  getProjectContextForActor,
  getProjectForOrganizationBySlug,
  listEnvironmentsForProject,
  listEnvironmentsForProjects,
  listOrganizationsForActor,
  listPreferencesForUserAndProjects,
  listProjectsForOrganization,
  updateActorSessionsOrganization,
  upsertUserProjectPreference,
} from "./workspace-repository.server";
import { requireOrganizationForActor } from "./authoring-repository.server";

function isUniqueViolation(cause: unknown) {
  const sqlError = sqlErrorFrom(cause);
  return sqlError !== undefined && sqlError.reason._tag === "UniqueViolation";
}

function generateEmptyProjectName() {
  return uniqueNamesGenerator({
    dictionaries: [adjectives, animals],
    separator: "-",
    length: 2,
    style: "lowerCase",
  });
}

const requireProjectContext = Effect.fn("EnvironmentDesign.requireProject")(
  function* (
    actor: Actor,
    input: { readonly organizationSlug: string; readonly projectSlug: string },
  ) {
    const context = yield* getProjectContextForActor(actor, input);
    if (context === null) {
      return yield* new NotFound({ message: "Project not found." });
    }
    return context;
  },
);

export const getOrganizationState = Effect.fn(
  "EnvironmentDesign.getOrganizationState",
)(function* (actor: Actor, organizationSlug?: string) {
  const organizations = yield* listOrganizationsForActor(actor);
  return {
    activeOrganization:
      organizations.find((organization) => organization.slug === organizationSlug) ??
      organizations[0] ??
      null,
    organizations,
  };
});

export const syncOrganizationSlug = Effect.fn(
  "EnvironmentDesign.syncOrganizationSlug",
)(function* (actor: Actor, input: SyncOrganizationSlug) {
  const organization = yield* requireOrganizationForActor(actor, input.organizationSlug);
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

export const listProjects = Effect.fn("EnvironmentDesign.listProjects")(
  function* (actor: Actor, input: ProjectList) {
    const organization = yield* requireOrganizationForActor(
      actor,
      input.organizationSlug,
    );
    const projects = yield* listProjectsForOrganization(organization.id);
    if (projects.length === 0) return [];
    const projectIds = projects.map((project) => project.id);
    const [environments, preferences] = yield* Effect.all(
      [
        listEnvironmentsForProjects(projectIds),
        listPreferencesForUserAndProjects(actor.userId, projectIds),
      ],
      { concurrency: "unbounded" },
    );
    const environmentsByProjectId = new Map<string, typeof environments>();
    for (const environment of environments) {
      const values = environmentsByProjectId.get(environment.projectId) ?? [];
      environmentsByProjectId.set(environment.projectId, [...values, environment]);
    }
    const preferenceByProjectId = new Map(
      preferences.map((preference) => [preference.projectId, preference]),
    );
    return projects.map((project) => {
      const projectEnvironments = environmentsByProjectId.get(project.id) ?? [];
      const firstEnvironment = projectEnvironments[0] ?? null;
      const preferredId = preferenceByProjectId.get(project.id)?.environmentId;
      const preferredEnvironment =
        projectEnvironments.find((environment) => environment.id === preferredId) ??
        null;
      return {
        ...project,
        firstEnvironment,
        userDefaultEnvironmentId: preferredEnvironment?.id ?? null,
        resolvedEnvironment: preferredEnvironment ?? firstEnvironment,
      };
    });
  },
);

export const getProjectBySlug = Effect.fn(
  "EnvironmentDesign.getProjectBySlug",
)(function* (actor: Actor, input: ProjectBySlug) {
  const organization = yield* requireOrganizationForActor(actor, input.organizationSlug);
  const project = yield* getProjectForOrganizationBySlug(
    organization.id,
    input.projectSlug,
  );
  if (project === null) {
    return yield* new NotFound({ message: "Project not found." });
  }
  return project;
});

export const createEmptyProject = Effect.fn(
  "EnvironmentDesign.createEmptyProject",
)(function* (actor: Actor, input: ProjectList) {
  const organization = yield* requireOrganizationForActor(actor, input.organizationSlug);
  return yield* withMutationResult(
    Effect.gen(function* () {
      const project = yield* createProject({
        organizationId: organization.id,
        name: generateEmptyProjectName(),
      });
      const environment = yield* createEnvironmentRecord({
        projectId: project.id,
        organizationId: organization.id,
        name: DEFAULT_ENVIRONMENT_NAME,
        namespace: createCanonicalEnvironmentNamespace({
          projectSlug: project.slug,
          environmentName: DEFAULT_ENVIRONMENT_NAME,
        }),
      });
      yield* upsertUserProjectPreference({
        userId: actor.userId,
        projectId: project.id,
        environmentId: environment.id,
      });
      return { project, environment };
    }),
  );
});

export const getEnvironmentBySlug = Effect.fn(
  "EnvironmentDesign.getEnvironmentBySlug",
)(function* (actor: Actor, input: EnvironmentBySlug) {
  const context = yield* requireProjectContext(actor, input);
  const environment = yield* getEnvironmentForProjectByNamespace(
    context.project.id,
    input.environmentSlug,
  );
  if (environment === null) {
    return yield* new NotFound({ message: "Environment not found." });
  }
  return environment;
});

export const selectEnvironment = Effect.fn("EnvironmentDesign.selectEnvironment")(
  function* (actor: Actor, input: EnvironmentBySlug) {
    const environment = yield* getEnvironmentBySlug(actor, input);
    yield* upsertUserProjectPreference({
      userId: actor.userId,
      projectId: environment.projectId,
      environmentId: environment.id,
    });
    return environment;
  },
);

export const listEnvironments = Effect.fn(
  "EnvironmentDesign.listEnvironments",
)(function* (actor: Actor, input: EnvironmentList) {
  const context = yield* requireProjectContext(actor, input);
  return yield* listEnvironmentsForProject(context.project.id);
});

export const resolvePreferredEnvironment = Effect.fn(
  "EnvironmentDesign.resolvePreferredEnvironment",
)(function* (actor: Actor, input: EnvironmentList) {
  const context = yield* requireProjectContext(actor, input);
  const [environments, preference] = yield* Effect.all(
    [
      listEnvironmentsForProject(context.project.id),
      getPreferenceForUserAndProject(actor.userId, context.project.id),
    ],
    { concurrency: "unbounded" },
  );
  const first = environments[0];
  if (first === undefined) {
    return yield* new NotFound({ message: "Environment not found." });
  }
  return (
    environments.find((environment) => environment.id === preference?.environmentId) ??
    first
  );
});

export const createEnvironment = Effect.fn(
  "EnvironmentDesign.createEnvironment",
)(function* (actor: Actor, input: CreateEnvironment) {
  const context = yield* requireProjectContext(actor, input);
  const create = withMutationResult(
    createEnvironmentRecord({
      projectId: context.project.id,
      organizationId: context.organization.id,
      name: input.name,
      namespace: createCanonicalEnvironmentNamespace({
        projectSlug: context.project.slug,
        environmentName: input.name,
      }),
    }),
  );
  return yield* create.pipe(
    Effect.catchIf(isUniqueViolation, () =>
      new Conflict({
        message: "Environment namespace already exists in this organization.",
      }),
    ),
  );
});
