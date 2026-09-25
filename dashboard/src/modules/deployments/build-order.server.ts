import "@tanstack/react-start/server-only";
import type { MachineId } from "@ployz/sdk";
import { eq } from "drizzle-orm";
import { Effect, Option, Schema } from "effect";
import { servicePolicySchema } from "#/modules/environment-design/service-policy";
import { service } from "#/modules/environment-design/tables";
import { checkGithubBuildWorkflow, listOrganizationGithubRepositories } from "#/modules/github/github-build.server";
import type { Actor } from "#/modules/identity/actor";
import { requireInfrastructureOrganization } from "#/modules/runtime/organization-access.server";
import { OrganizationRuntime, RUNTIME_FRAME_TIMEOUT_MS } from "#/modules/runtime/organization-runtime.server";
import { Database } from "#/server/database.server";
import { defaultBuildOrder, imageBuildWalk, type BuildOrder, type BuildOrderRow } from "./build-order";
import type { SkipReason } from "./image-build";
import { skipUnstarted, type ImageBuildTarget } from "./image-builds.server";
import { environmentDeployment, organizationBuildOrder } from "./tables";

/** Whether GitHub is set up: some repository the Organization's Services build from has the build workflow. */
const githubBuildsSetUp = Effect.fn("Deployments.githubBuildsSetUp")(function* (organizationId: string) {
  for (const repository of yield* listOrganizationGithubRepositories(organizationId)) {
    const workflow = yield* checkGithubBuildWorkflow(repository.installationId, repository.repositoryId);
    if (workflow.readiness === "ready") return true;
  }
  return false;
}, Effect.orElseSucceed(() => false));

/** Read when an Image Build starts, so a change (or GitHub's setup, for the default) takes effect on the next build. */
export const loadBuildOrder = Effect.fn("Deployments.loadBuildOrder")(function* (organizationId: string) {
  const { drizzle } = yield* Database;
  const [row] = yield* drizzle.select({ buildOrder: organizationBuildOrder.buildOrder }).from(organizationBuildOrder)
    .where(eq(organizationBuildOrder.organizationId, organizationId)).limit(1);
  return row?.buildOrder ?? defaultBuildOrder(yield* githubBuildsSetUp(organizationId));
});

/**
 * Why a Preferred Server can't take builds any more, from the Cluster's current view; null while it
 * can. A Cluster Cloud can't see right now keeps the preference: the Engine ranks it anyway, and
 * reports it unavailable if it vanished since.
 */
const preferredServerUnavailable = Effect.fn("Deployments.preferredServerUnavailable")(function* (organizationId: string, machineId: MachineId) {
  const session = yield* (yield* OrganizationRuntime).open(organizationId);
  if (session.status !== "connected") return null;
  const frame = yield* session.connected.watchFirstFrame(RUNTIME_FRAME_TIMEOUT_MS);
  const observed = frame.machines.find(({ machine }) => machine.id === machineId);
  if (observed?.machine.accepts_builds) return null;
  return { builder: "servers", kind: "preferred_unavailable", machineId, name: observed?.machine.name ?? null } satisfies SkipReason;
}, Effect.scoped, Effect.orElseSucceed(() => null));

/**
 * The Builders one Image Build tries, in turn: the Service's Preferred Builder, then the
 * Organization's Build Order without it, both read as the build starts. A Preferred Server that is
 * gone or no longer builds goes back to Auto, and the skip trail says why, so a GitHub-only
 * Organization never builds on its servers.
 */
export const planImageBuildWalk = Effect.fn("Deployments.planImageBuildWalk")(function* (
  build: Pick<ImageBuildTarget, "id" | "image" | "deploymentId" | "serviceId">,
) {
  const { drizzle } = yield* Database;
  const [row] = yield* drizzle.select({ organizationId: environmentDeployment.organizationId, policy: service.policy })
    .from(environmentDeployment).leftJoin(service, eq(service.id, build.serviceId))
    .where(eq(environmentDeployment.id, build.deploymentId)).limit(1);
  if (!row) return imageBuildWalk(defaultBuildOrder(false), undefined);
  const policy = Schema.decodeUnknownOption(servicePolicySchema)(row.policy);
  if (row.policy !== null && Option.isNone(policy)) {
    yield* Effect.logWarning("A Service's policy does not decode; its build follows the Build Order.", { serviceId: build.serviceId });
  }
  let preferred = Option.isSome(policy) ? policy.value.preferredBuilder : undefined;
  if (preferred !== undefined && preferred !== "github") {
    const unavailable = yield* preferredServerUnavailable(row.organizationId, preferred);
    if (unavailable) {
      yield* skipUnstarted(build, unavailable);
      preferred = undefined;
    }
  }
  return imageBuildWalk(yield* loadBuildOrder(row.organizationId), preferred);
});

export const setBuildOrder = Effect.fn("Deployments.setBuildOrder")(function* (
  actor: Actor,
  input: { organizationSlug: string; buildOrder: BuildOrder },
) {
  const organization = yield* requireInfrastructureOrganization(actor, input.organizationSlug);
  const { drizzle } = yield* Database;
  yield* drizzle.insert(organizationBuildOrder).values({ organizationId: organization.id, buildOrder: input.buildOrder })
    .onConflictDoUpdate({ target: organizationBuildOrder.organizationId, set: { buildOrder: input.buildOrder, updatedAt: new Date() } });
  const row: BuildOrderRow = { id: organization.id, buildOrder: input.buildOrder };
  return row;
});
