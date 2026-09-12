import { createServerFn } from "@tanstack/react-start";
import { Schema } from "effect";
import {
  actorMiddleware,
  publicErrorMiddleware,
  runActor,
  strictValidator,
} from "#/server/tanstack";
import {
  createEmptyProject,
  createEnvironment,
  getEnvironmentBySlug,
  getOrganizationState,
  getProjectBySlug,
  listEnvironments,
  listProjects,
  resolvePreferredEnvironment,
  selectEnvironment,
  syncOrganizationSlug,
} from "./workspace-operations.server";
import {
  CreateEnvironment,
  EnvironmentBySlug,
  EnvironmentList,
  OrganizationSlug,
  ProjectBySlug,
  ProjectList,
  SyncOrganizationSlug,
} from "./workspace-schemas";

const middleware = [publicErrorMiddleware, actorMiddleware] as const;

const OrganizationStateInput = Schema.Struct({
  organizationSlug: Schema.optionalKey(OrganizationSlug),
});

export const getOrganizationStateServerFn = createServerFn({ method: "GET" })
  .middleware(middleware)
  .validator(strictValidator(OrganizationStateInput))
  .handler(({ context, data }) =>
    runActor(context, getOrganizationState(context.actor, data.organizationSlug)),
  );

export const syncOrganizationSlugServerFn = createServerFn({ method: "POST" })
  .middleware(middleware)
  .validator(strictValidator(SyncOrganizationSlug))
  .handler(({ context, data }) =>
    runActor(context, syncOrganizationSlug(context.actor, data)),
  );

export const listProjectsServerFn = createServerFn({ method: "GET" })
  .middleware(middleware)
  .validator(strictValidator(ProjectList))
  .handler(({ context, data }) =>
    runActor(context, listProjects(context.actor, data)),
  );

export const getProjectBySlugServerFn = createServerFn({ method: "GET" })
  .middleware(middleware)
  .validator(strictValidator(ProjectBySlug))
  .handler(({ context, data }) =>
    runActor(context, getProjectBySlug(context.actor, data)),
  );

export const createEmptyProjectServerFn = createServerFn({ method: "POST" })
  .middleware(middleware)
  .validator(strictValidator(ProjectList))
  .handler(({ context, data }) =>
    runActor(context, createEmptyProject(context.actor, data)),
  );

export const getEnvironmentBySlugServerFn = createServerFn({ method: "GET" })
  .middleware(middleware)
  .validator(strictValidator(EnvironmentBySlug))
  .handler(({ context, data }) =>
    runActor(context, getEnvironmentBySlug(context.actor, data)),
  );

export const resolvePreferredEnvironmentServerFn = createServerFn({
  method: "GET",
})
  .middleware(middleware)
  .validator(strictValidator(EnvironmentList))
  .handler(({ context, data }) =>
    runActor(context, resolvePreferredEnvironment(context.actor, data)),
  );

export const listEnvironmentsServerFn = createServerFn({ method: "GET" })
  .middleware(middleware)
  .validator(strictValidator(EnvironmentList))
  .handler(({ context, data }) =>
    runActor(context, listEnvironments(context.actor, data)),
  );

export const createEnvironmentServerFn = createServerFn({ method: "POST" })
  .middleware(middleware)
  .validator(strictValidator(CreateEnvironment))
  .handler(({ context, data }) =>
    runActor(context, createEnvironment(context.actor, data)),
  );

export const selectEnvironmentServerFn = createServerFn({ method: "POST" })
  .middleware(middleware)
  .validator(strictValidator(EnvironmentBySlug))
  .handler(({ context, data }) =>
    runActor(context, selectEnvironment(context.actor, data)),
  );
