import "@tanstack/react-start/server-only";

import { and, eq, inArray } from "drizzle-orm";
import { Effect } from "effect";
import {
  project as schemaProject,
  environment as schemaEnvironment,
  environmentBranch as schemaEnvironmentBranch,
} from "#/modules/project/tables";
import { organization as schemaOrganization } from "#/modules/organization/tables";
import { descendants } from "#/modules/project/environment-tree";
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
  defaultEnvironmentRefusal,
  type ConfirmTeardownInput,
  type RetryTeardownInput,
  type TeardownAttemptScope,
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
import { afterDatabaseCommit, Database } from "#/server/database.server";
import { lockEnvironmentDeploymentQueues } from "#/modules/deployments/queue-lock.server";
import { lockOrganizationProjects, lockProjectDefault } from "#/modules/environment-design/workspace-repository.server";
import { Conflict, NotFound, Validation } from "#/server/public-error";
import { disableOrganizationPairing } from "#/modules/machines/pairing-removal.server";

type OrganizationRecord = { readonly id: string; readonly slug: string };
type ProjectRecord = {
  readonly id: string;
  readonly slug: string;
  readonly name: string;
  readonly defaultEnvironmentId: string | null;
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
          defaultEnvironmentId: schemaProject.defaultEnvironmentId,
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
    return yield* loadEnvironmentAccess(input.environmentId, organization.id);
  },
);

/** An Environment's teardown access, within `organizationId`. */
const loadEnvironmentAccess = Effect.fn("Teardown.loadEnvironmentAccess")(
  function* (environmentId: string, organizationId: string) {
    const database = yield* Database;
    const rows = yield* database.drizzle
      .select({
        organization: { id: schemaOrganization.id, slug: schemaOrganization.slug },
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
          defaultEnvironmentId: schemaProject.defaultEnvironmentId,
        },
      })
      .from(schemaEnvironment)
      .innerJoin(schemaProject, eq(schemaEnvironment.projectId, schemaProject.id))
      .innerJoin(schemaOrganization, eq(schemaProject.organizationId, schemaOrganization.id))
      .where(
        and(
          eq(schemaEnvironment.id, environmentId),
          eq(schemaProject.organizationId, organizationId),
        ),
      )
      .limit(1);
    const found = rows[0];
    if (found !== undefined) {
      return { scope: "environment", ...found } satisfies TeardownAccess;
    }
    return yield* new NotFound({
      message: "The environment was not found.",
    });
  },
);

/**
 * What a teardown removes. An Environment takes its Branches with it, deepest first, in the same attempt: the runtime
 * destroys them in that order and the Cloud rows go in one statement, so no Branch outlives its Parent.
 */
const loadTeardownGraph = Effect.fn("Teardown.loadGraph")(function* (
  access: TeardownAccess,
) {
  const database = yield* Database;
  if (access.scope === "environment") {
    const branches = yield* database.drizzle
      .select({
        environmentId: schemaEnvironmentBranch.environmentId,
        parentEnvironmentId: schemaEnvironmentBranch.parentEnvironmentId,
      })
      .from(schemaEnvironmentBranch)
      .where(eq(schemaEnvironmentBranch.projectId, access.project.id));
    const closing = descendants(access.environment.id, branches);
    const rows = closing.length === 0
      ? []
      : yield* database.drizzle
          .select({
            id: schemaEnvironment.id,
            projectId: schemaEnvironment.projectId,
            name: schemaEnvironment.name,
            namespace: schemaEnvironment.namespace,
          })
          .from(schemaEnvironment)
          .where(inArray(schemaEnvironment.id, closing));
    const environments = [
      ...closing.flatMap((id) => rows.filter((row) => row.id === id)),
      access.environment,
    ].map((environment) => ({ ...environment, projectSlug: access.project.slug }));
    const defaultEnvironment = environments.find(
      (environment) => environment.id === access.project.defaultEnvironmentId,
    );
    if (defaultEnvironment !== undefined) {
      return yield* new Conflict({
        message: defaultEnvironmentRefusal(defaultEnvironment.name),
        userFacing: true,
      });
    }
    return { projects: [access.project], environments };
  }
  const projects =
    access.scope === "project"
      ? [access.project]
      : yield* database.drizzle
          .select({
            id: schemaProject.id,
            slug: schemaProject.slug,
            name: schemaProject.name,
            defaultEnvironmentId: schemaProject.defaultEnvironmentId,
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

type TeardownGraph = Effect.Success<ReturnType<typeof loadTeardownGraph>>;

const loadCatalog = Effect.fn("Teardown.loadCatalog")(function* (
  environmentIds: readonly string[],
) {
  if (environmentIds.length === 0) return { services: [], volumes: [] };
  const database = yield* Database;
  const documents = yield* database.drizzle.select({ id: schemaEnvironment.id, intent: schemaEnvironment.intent })
    .from(schemaEnvironment).where(inArray(schemaEnvironment.id, [...environmentIds]));
  const services = documents.flatMap((document) => document.intent.services.map((node) => ({ environmentId: document.id, name: node.config.privateDns })));
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

/** The organization's runtime; a project or Environment teardown refuses one it can't reach. */
const reachableRuntime = Effect.fn("Teardown.reachableRuntime")(function* (access: TeardownAccess) {
  const runtime = yield* inspectRuntime(access.organization.id);
  if (access.scope !== "organization" && runtime.cluster.kind === "unreachable") {
    return yield* new Validation({
      message: "Can't reach your servers. Check they're online, then try again.",
      userFacing: true,
    });
  }
  return runtime;
});
type ReachableRuntime = Effect.Success<ReturnType<typeof reachableRuntime>>;

const teardownDataLoss = Effect.fn("Teardown.dataLoss")(function* (
  access: TeardownAccess,
  graph: TeardownGraph,
  runtime: ReachableRuntime,
) {
  const catalog = yield* loadCatalog(
    graph.environments.map((environment) => environment.id),
  );
  const organizationSlug = access.organization.slug;
  const environmentLists = graph.environments.map((environment) =>
    dataLossForEnvironment({
      organizationSlug,
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
            organizationSlug,
            projectSlug: project.slug,
          }),
        );
  const organizationLists =
    access.scope === "organization"
      ? [organizationCloudRow(organizationSlug)]
      : [];
  return unionDataLossLists([
    ...environmentLists,
    { rust, cloud: [] },
    ...projectLists,
    ...organizationLists,
  ]);
});

export const loadTeardownDataLoss = Effect.fn("Teardown.loadDataLoss")(
  function* (actor: Actor, input: TeardownTargetInput) {
    const access = yield* requireTeardownAccess(actor, input);
    return yield* teardownDataLoss(access, yield* loadTeardownGraph(access), yield* reachableRuntime(access));
  },
);

export const dispatchTeardownRequested = Effect.fn("Teardown.dispatchRequested")(
  function* (attemptId: string) {
    yield* sendInngestEvent(createTeardownRequestedEvent({ attemptId }));
  },
);

const startTeardown = Effect.fn("Teardown.start")(function* (
  access: TeardownAccess,
  graph: TeardownGraph,
  runtime: ReachableRuntime,
  input: {
    readonly requestedByUserId: string;
    readonly identities: ConfirmTeardownInput["identities"];
    readonly abandon: boolean;
    readonly scope: TeardownAttemptScope;
  },
) {
  const plan = planTeardownRuntime({
    scope: access.scope,
    abandon: input.abandon,
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
      requestedByUserId: input.requestedByUserId,
      projectId: access.scope === "organization" ? null : access.project.id,
      environmentId:
        access.scope === "environment" ? access.environment.id : null,
      scope: input.scope,
      confirmDataLoss: input.identities,
      targets,
    });
  if (access.scope === "organization") yield* disableOrganizationPairing(access.organization.id);
  // Inside a transaction (the idle sweep's), the event waits for the commit.
  yield* afterDatabaseCommit(dispatchTeardownRequested(attempt.id));
  return attempt;
});

/**
 * Admits a teardown against its graph as it is now. A project or Environment teardown holds the Project row and reads its
 * Default Environment under it, so choosing a Default waits for the teardown, and a Default chosen before it (even after
 * `access` was read) is refused by loadTeardownGraph. With `expected`, the graph must still hold exactly those Environments.
 */
const admitTeardown = Effect.fn("Teardown.admit")(function* (
  access: TeardownAccess,
  runtime: ReachableRuntime,
  input: Omit<Parameters<typeof startTeardown>[3], "scope"> & { readonly expected?: readonly string[] },
) {
  const database = yield* Database;
  return yield* database.transaction(Effect.gen(function* () {
    if (access.scope === "organization") yield* lockOrganizationProjects(access.organization.id);
    const current = access.scope === "organization" ? access
      : { ...access, project: { ...access.project, defaultEnvironmentId: yield* lockProjectDefault(access.project.id) } };
    const graph = yield* loadTeardownGraph(current);
    if (input.expected && graph.environments.map((environment) => environment.id).join() !== input.expected.join()) {
      return yield* new Conflict({ message: "What this teardown removes changed. Try again." });
    }
    // Each Environment's deployment queue, by id: deployment admission waits, then sees this teardown and refuses. A
    // caller that holds documents took these first (lock order: lockProjectDefault), so here they don't wait.
    yield* lockEnvironmentDeploymentQueues(graph.environments.map((environment) => environment.id));
    return yield* startTeardown(current, graph, runtime, { ...input, scope: current.scope });
  }));
});

export const confirmTeardown = Effect.fn("Teardown.confirm")(
  function* (actor: Actor, input: ConfirmTeardownInput) {
    const access = yield* requireTeardownAccess(actor, input);
    return yield* admitTeardown(access, yield* reachableRuntime(access), {
      requestedByUserId: actor.userId,
      identities: input.identities,
      abandon: input.abandon === true,
    });
  },
);

/**
 * Tears down an Environment and its Branches with no one confirming, in two steps: prepare asks the runtime (holding no
 * locks) for the data-loss report of their namespaces, and admit confirms exactly that. An unreachable runtime refuses;
 * the caller decides when to try again.
 */
export const prepareSystemTeardown = Effect.fn("Teardown.prepareSystem")(
  function* (input: { readonly organizationId: string; readonly environmentId: string }) {
    const access = yield* loadEnvironmentAccess(input.environmentId, input.organizationId);
    const graph = yield* loadTeardownGraph(access);
    const runtime = yield* reachableRuntime(access);
    const dataLoss = yield* teardownDataLoss(access, graph, runtime);
    return { access, runtime, expected: graph.environments.map((environment) => environment.id), identities: dataLoss.rust };
  },
);

/**
 * Shuts one Environment down with no one confirming: its runtime half only, every row kept. Prepare asks the runtime
 * (holding no locks) for the data-loss report of its namespace alone, not its Branches'; `admit` confirms exactly that,
 * as `requestedByUserId`: database work only, safe under a caller's locks, which must include its deployment queue.
 */
export const prepareShutdown = Effect.fn("Teardown.prepareShutdown")(
  function* (input: { readonly organizationId: string; readonly environmentId: string }) {
    const access = yield* loadEnvironmentAccess(input.environmentId, input.organizationId);
    const graph = { projects: [access.project], environments: [{ ...access.environment, projectSlug: access.project.slug }] };
    const runtime = yield* reachableRuntime(access);
    const dataLoss = yield* teardownDataLoss(access, graph, runtime);
    return {
      admit: (requestedByUserId: string) => startTeardown(access, graph, runtime, {
        requestedByUserId, identities: dataLoss.rust, abandon: false, scope: "shutdown",
      }),
    };
  },
);

/**
 * Admits a prepared system teardown as `requestedByUserId`: database work only, safe under a caller's locks. Refuses when
 * the Environments it removes changed since it was prepared.
 */
export const admitSystemTeardown = Effect.fn("Teardown.admitSystem")(function* (
  prepared: Effect.Success<ReturnType<typeof prepareSystemTeardown>>,
  requestedByUserId: string,
) {
  return yield* admitTeardown(prepared.access, prepared.runtime, {
    requestedByUserId, identities: prepared.identities, abandon: false, expected: prepared.expected,
  });
});

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
