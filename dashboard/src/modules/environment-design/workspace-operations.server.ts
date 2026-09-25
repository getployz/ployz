import "@tanstack/react-start/server-only";
import { adjectives, animals, uniqueNamesGenerator } from "unique-names-generator";
import { Effect } from "effect";
import { isUniqueViolation } from "#/server/database.server";
import { Conflict, NotFound } from "#/server/public-error";
import type { Actor } from "#/modules/identity/actor";
import { withMutationResult } from "#/server/mutation-result.server";
import {
  createCanonicalEnvironmentNamespace,
  DEFAULT_ENVIRONMENT_NAME,
  type CreateEnvironment,
  type EnvironmentBySlug,
  type ProjectList,
  type SyncOrganizationSlug,
} from "./workspace-schemas";
import {
  createEnvironmentRecord,
  createProject,
  getEnvironmentForProjectByNamespace,
  getProjectContextForActor,
  listOrganizationsForActor,
  updateActorSessionsOrganization,
  upsertUserProjectPreference,
} from "./workspace-repository.server";
import { requireOrganizationForActor } from "./authoring-repository.server";
import { Polar } from "#/modules/billing/polar-provider.server";

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
