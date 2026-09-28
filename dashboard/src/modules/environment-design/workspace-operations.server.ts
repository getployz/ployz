import "@tanstack/react-start/server-only";
import { adjectives, animals, uniqueNamesGenerator } from "unique-names-generator";
import { Effect } from "effect";
import { Database, isUniqueViolation } from "#/server/database.server";
import { activeTeardownFor } from "#/modules/runtime/teardown.repository";
import { Conflict, NotFound, Validation } from "#/server/public-error";
import { isPrEnvironment } from "#/modules/pr-environments/pr-environment.repository.server";
import type { Actor } from "#/modules/identity/actor";
import { withMutationResult } from "#/server/mutation-result.server";
import {
  createCanonicalEnvironmentNamespace,
  DEFAULT_ENVIRONMENT_NAME,
  type CreateEnvironment,
  type ProjectList,
  type SetDefaultEnvironment,
  type SyncOrganizationSlug,
} from "./workspace-schemas";
import {
  createEnvironmentRecord,
  createProject,
  getProjectContextForActor,
  listOrganizationsForActor,
  lockProjectDefault,
  setDefaultEnvironment,
  updateActorSessionsOrganization,
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
      const created = yield* createProject({
        organizationId: organization.id,
        name: generateEmptyProjectName(),
      });
      const environment = yield* createEnvironmentRecord({
        projectId: created.id,
        organizationId: organization.id,
        name: DEFAULT_ENVIRONMENT_NAME,
        namespace: createCanonicalEnvironmentNamespace({
          projectSlug: created.slug,
          environmentName: DEFAULT_ENVIRONMENT_NAME,
        }),
      });
      const project = yield* setDefaultEnvironment(created.id, environment.id);
      if (project === null) {
        return yield* Effect.die("PostgreSQL did not return the updated project.");
      }
      return { project, environment };
    }),
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

export const setProjectDefaultEnvironment = Effect.fn(
  "EnvironmentDesign.setProjectDefaultEnvironment",
)(function* (actor: Actor, input: SetDefaultEnvironment) {
  const context = yield* requireProjectContext(actor, input);
  const database = yield* Database;
  // Under the Project row, so an idle close admitting this Environment's teardown either sees the new default or runs first.
  return yield* database.transaction(Effect.gen(function* () {
    yield* lockProjectDefault(context.project.id);
    if ((yield* activeTeardownFor([input.environmentId])).size > 0) return yield* new Conflict({ message: "This environment is being torn down." });
    if (yield* isPrEnvironment(input.environmentId)) return yield* new Validation({ message: "A PR environment can't be the Default Environment." });
    const project = yield* setDefaultEnvironment(context.project.id, input.environmentId);
    if (project === null) {
      return yield* new NotFound({ message: "Environment not found in this project." });
    }
    return project;
  }));
});
