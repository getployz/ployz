import "@tanstack/react-start/server-only";

import { and, eq, inArray } from "drizzle-orm";
import { Effect } from "effect";
import {
  project as schemaProject,
  environment as schemaEnvironment,
} from "#/modules/project/tables";
import { sendInngestEvent } from "#/modules/inngest/client";
import { createTeardownRequestedEvent } from "#/modules/inngest/events";
import {
  unionDataLossLists,
  type DataLossList,
} from "#/modules/runtime/data-loss-confirm";
import type { Actor } from "#/modules/identity/actor";
import { requireInfrastructureOrganization } from "#/modules/runtime/organization-access.server";
import { type PloyzSession } from "#/modules/runtime/ployz.server";
import { OrganizationRuntime } from "#/modules/runtime/organization-runtime.server";
import {
  cloudEnvironmentName,
  environmentCloudRows,
  organizationCloudRow,
  planTeardownRuntime,
  projectCloudRow,
  retryPlanForAttempt,
  teardownRuntimeRefuseMessage,
  type ConfirmTeardownInput,
  type RetryTeardownInput,
  type TeardownClusterView,
  type TeardownRuntimePlan,
  type TeardownTargetInput,
  type TeardownTargets,
} from "#/modules/runtime/teardown";
import {
  insertTeardownAttempt,
  loadLatestTeardownAttemptForScope,
  loadTeardownAttempt,
  type TeardownAttempt,
} from "#/modules/runtime/teardown.repository";
import { Database } from "#/server/database.server";
import { Conflict, NotFound, Validation } from "#/server/public-error";
import { disableOrganizationPairing } from "#/modules/machines/pairing-removal.server";

type OrganizationRecord = { readonly id: string; readonly slug: string };
type ProjectRecord = {
  readonly id: string;
  readonly slug: string;
  readonly name: string;
};
type EnvironmentRecord = {
  readonly id: string;
  readonly projectId: string;
  readonly name: string;
  readonly namespace: string;
};
type EnvironmentWithProject = EnvironmentRecord & {
  readonly projectSlug: string;
};

type TeardownAccess =
  | {
      readonly scope: "environment";
      readonly organization: OrganizationRecord;
      readonly project: ProjectRecord;
      readonly environment: EnvironmentRecord;
    }
  | {
      readonly scope: "project";
      readonly organization: OrganizationRecord;
      readonly project: ProjectRecord;
    }
  | {
      readonly scope: "organization";
      readonly organization: OrganizationRecord;
    };

const requireTeardownAccess = Effect.fn("Teardown.requireAccess")(
  function* (actor: Actor, input: TeardownTargetInput) {
    const organization = yield* requireInfrastructureOrganization(
      actor,
      input.organizationSlug,
    );
    if (input.scope === "organization") {
      return { scope: "organization", organization } satisfies TeardownAccess;
    }
    const database = yield* Database;
    if (input.scope === "project") {
      if (input.projectSlug === undefined) {
        return yield* new Validation({
          field: "projectSlug",
          message: "Project teardown needs a project.",
        });
      }
      const rows = yield* database.drizzle
        .select({
          id: schemaProject.id,
          slug: schemaProject.slug,
          name: schemaProject.name,
        })
        .from(schemaProject)
        .where(
          and(
            eq(schemaProject.organizationId, organization.id),
            eq(schemaProject.slug, input.projectSlug),
          ),
        )
        .limit(1);
      const project = rows[0];
      if (project !== undefined) {
        return { scope: "project", organization, project } satisfies TeardownAccess;
      }
      return yield* new NotFound({
        message: "The project was not found.",
      });
    }

    if (input.environmentId === undefined) {
      return yield* new Validation({
        field: "environmentId",
        message: "Environment teardown needs an environment.",
      });
    }
    const rows = yield* database.drizzle
      .select({
        environment: {
          id: schemaEnvironment.id,
          projectId: schemaEnvironment.projectId,
          name: schemaEnvironment.name,
          namespace: schemaEnvironment.namespace,
        },
        project: {
          id: schemaProject.id,
          slug: schemaProject.slug,
          name: schemaProject.name,
        },
      })
      .from(schemaEnvironment)
      .innerJoin(schemaProject, eq(schemaEnvironment.projectId, schemaProject.id))
      .where(
        and(
          eq(schemaEnvironment.id, input.environmentId),
          eq(schemaProject.organizationId, organization.id),
        ),
      )
      .limit(1);
    const found = rows[0];
    if (found !== undefined) {
      return {
        scope: "environment",
        organization,
        project: found.project,
        environment: found.environment,
      } satisfies TeardownAccess;
    }
    return yield* new NotFound({
      message: "The environment was not found.",
    });
  },
);

const loadTeardownGraph = Effect.fn("Teardown.loadGraph")(function* (
  access: TeardownAccess,
) {
  if (access.scope === "environment") {
    return {
      projects: [access.project],
      environments: [
        { ...access.environment, projectSlug: access.project.slug },
      ],
    };
  }
  const database = yield* Database;
  const projects =
    access.scope === "project"
      ? [access.project]
      : yield* database.drizzle
          .select({
            id: schemaProject.id,
            slug: schemaProject.slug,
            name: schemaProject.name,
          })
          .from(schemaProject)
          .where(eq(schemaProject.organizationId, access.organization.id));
  const projectIds = projects.map((project) => project.id);
  const environments =
    projectIds.length === 0
      ? []
      : yield* database.drizzle
          .select({
            id: schemaEnvironment.id,
            projectId: schemaEnvironment.projectId,
            name: schemaEnvironment.name,
            namespace: schemaEnvironment.namespace,
            projectSlug: schemaProject.slug,
          })
          .from(schemaEnvironment)
          .innerJoin(schemaProject, eq(schemaEnvironment.projectId, schemaProject.id))
          .where(inArray(schemaEnvironment.projectId, projectIds));
  return { projects, environments };
});

const loadCatalog = Effect.fn("Teardown.loadCatalog")(function* (
  environmentIds: readonly string[],
) {
  if (environmentIds.length === 0) return { services: [], volumes: [] };
  const database = yield* Database;
  const documents = yield* database.drizzle.select({ id: schemaEnvironment.id, intent: schemaEnvironment.intent })
    .from(schemaEnvironment).where(inArray(schemaEnvironment.id, [...environmentIds]));
  const services = documents.flatMap((document) => document.intent.services.map((node) => ({ environmentId: document.id, name: node.config.name })));
  const volumes = documents.flatMap((document) => document.intent.volumes.map((node) => ({ environmentId: document.id, name: node.name })));
  return { services, volumes };
});

type RuntimeInspection =
  | {
      readonly cluster: Extract<TeardownClusterView, { kind: "no_cluster" | "unreachable" }>;
    }
  | {
      readonly cluster: Extract<TeardownClusterView, { kind: "live" }>;
      readonly client: PloyzSession;
    };

function isConnectedRuntimeInspection(
  runtime: RuntimeInspection,
): runtime is Extract<RuntimeInspection, { client: PloyzSession }> {
  return runtime.cluster.kind === "live";
}

const inspectRuntime = Effect.fn("Teardown.inspectRuntime")(function* (
  organizationId: string,
) {
  const runtime = yield* OrganizationRuntime;
  const session = yield* runtime.open(organizationId);
  if (session.status === "no_connection") {
    return {
      cluster: { kind: "no_cluster" },
    } satisfies RuntimeInspection;
  }
  if (session.status === "unreachable") {
    return {
      cluster: { kind: "unreachable" },
    } satisfies RuntimeInspection;
  }
  return {
    cluster: { kind: "live" },
    client: session.connected,
  } satisfies RuntimeInspection;
});

function dataLossForEnvironment(input: {
  organizationSlug: string;
  projectSlug: string;
  environment: EnvironmentRecord;
  services: readonly { name: string }[];
  volumes: readonly { name: string }[];
}): DataLossList {
  const cloudName = cloudEnvironmentName({
    organizationSlug: input.organizationSlug,
    projectSlug: input.projectSlug,
    environmentName: input.environment.name,
  });
  return environmentCloudRows({
    cloudName,
    services: input.services,
    volumes: input.volumes,
  });
}

function targetsFor(
  access: TeardownAccess,
  environments: readonly EnvironmentWithProject[],
  plan: Extract<TeardownRuntimePlan, { kind: "ok" }>,
  runtime: RuntimeInspection,
): TeardownTargets {
  return {
    environments: environments.map((environment) => ({
      environmentId: environment.id,
      projectId: environment.projectId,
      projectName: environment.namespace,
      cloudName: cloudEnvironmentName({
        organizationSlug: access.organization.slug,
        projectSlug: environment.projectSlug,
        environmentName: environment.name,
      }),
    })),
    destroyRuntimeProjects:
      access.scope !== "organization" && isConnectedRuntimeInspection(runtime),
    revokePairing: access.scope === "organization" || plan.revokePairing,
    runtimeMembership: plan.runtimeMembership,
  };
}

export const loadTeardownDataLoss = Effect.fn("Teardown.loadDataLoss")(
  function* (actor: Actor, input: TeardownTargetInput) {
    const access = yield* requireTeardownAccess(actor, input);
    const graph = yield* loadTeardownGraph(access);
    const catalog = yield* loadCatalog(
      graph.environments.map((environment) => environment.id),
    );
    const runtime = yield* inspectRuntime(access.organization.id);
    if (
      access.scope !== "organization" &&
      runtime.cluster.kind === "unreachable"
    ) {
      return yield* new Validation({
        message: "The runtime is unreachable. Reconnect it before tearing down this target.",
      });
    }
    const environmentLists = graph.environments.map((environment) =>
      dataLossForEnvironment({
        organizationSlug: input.organizationSlug,
        projectSlug: environment.projectSlug,
        environment,
        services: catalog.services.filter(
          (service) => service.environmentId === environment.id,
        ),
        volumes: catalog.volumes.filter(
          (volume) => volume.environmentId === environment.id,
        ),
      }),
    );
    let rust: DataLossList["rust"] = [];
    if (isConnectedRuntimeInspection(runtime)) {
      if (access.scope === "organization") {
        rust = (yield* runtime.client.dataLossIfClusterDestroyed()).data_loss;
      } else {
        const observed = yield* Effect.all(
          graph.environments.map((environment) =>
            runtime.client.dataLossIfProjectDestroyed(environment.namespace, true),
          ),
        );
        rust = observed.flatMap((dataLoss) => dataLoss.data_loss);
      }
    }
    const projectLists =
      access.scope === "environment"
        ? []
        : graph.projects.map((project) =>
            projectCloudRow({
              organizationSlug: input.organizationSlug,
              projectSlug: project.slug,
            }),
          );
    const organizationLists =
      access.scope === "organization"
        ? [organizationCloudRow(input.organizationSlug)]
        : [];
    return unionDataLossLists([
      ...environmentLists,
      { rust, cloud: [] },
      ...projectLists,
      ...organizationLists,
    ]);
  },
);

export const dispatchTeardownRequested = Effect.fn("Teardown.dispatchRequested")(
  function* (attemptId: string) {
    yield* sendInngestEvent(createTeardownRequestedEvent({ attemptId }));
  },
);

export const confirmTeardown = Effect.fn("Teardown.confirm")(
  function* (actor: Actor, input: ConfirmTeardownInput) {
    const access = yield* requireTeardownAccess(actor, input);
    const graph = yield* loadTeardownGraph(access);
    const runtime = yield* inspectRuntime(access.organization.id);
    if (
      access.scope !== "organization" &&
      runtime.cluster.kind === "unreachable"
    ) {
      return yield* new Validation({
        message: "The runtime is unreachable. Reconnect it before tearing down this target.",
      });
    }
    const plan = planTeardownRuntime({
      scope: access.scope,
      abandon: input.abandon === true,
      cluster:
        access.scope === "organization"
          ? runtime.cluster
          : { kind: "no_cluster" },
    });
    if (plan.kind === "refuse") {
      return yield* new Validation({
        message: teardownRuntimeRefuseMessage(plan.reason),
      });
    }
    const targets = targetsFor(access, graph.environments, plan, runtime);
    const attempt = yield*
      insertTeardownAttempt({
        organizationId: access.organization.id,
        requestedByUserId: actor.userId,
        projectId: access.scope === "organization" ? null : access.project.id,
        environmentId:
          access.scope === "environment" ? access.environment.id : null,
        scope: access.scope,
        confirmDataLoss: input.identities,
        targets,
      });
    if (access.scope === "organization") yield* disableOrganizationPairing(access.organization.id);
    yield* dispatchTeardownRequested(attempt.id);
    return attempt;
  },
);

export const retryTeardown = Effect.fn("Teardown.retry")(
  function* (actor: Actor, input: RetryTeardownInput) {
    const organization = yield* requireInfrastructureOrganization(
      actor,
      input.organizationSlug,
    );
    const attempt = yield* loadTeardownAttempt(input.attemptId);
    if (attempt === null || attempt.organizationId !== organization.id) {
      return yield* new NotFound({
        message: "The teardown attempt was not found.",
      });
    }
    const retry = retryPlanForAttempt(attempt.status);
    if (retry.kind === "conflict") {
      return yield* new Conflict({
        message: "This teardown cannot be retried yet.",
      });
    }
    yield* dispatchTeardownRequested(attempt.id);
    return attempt;
  },
);

export const loadLatestTeardownAttempt = Effect.fn("Teardown.loadLatest")(
  function* (actor: Actor, input: TeardownTargetInput) {
    const access = yield* requireTeardownAccess(actor, input);
    return yield*
      loadLatestTeardownAttemptForScope({
        organizationId: access.organization.id,
        scope: input.scope,
        projectId: access.scope === "organization" ? null : access.project.id,
        environmentId:
          access.scope === "environment" ? access.environment.id : null,
      });
  },
);

export type { TeardownAttempt };
