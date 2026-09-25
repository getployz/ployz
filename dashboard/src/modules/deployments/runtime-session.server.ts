import "@tanstack/react-start/server-only";
import type { MachineId } from "@ployz/sdk";
import { Data, Effect } from "effect";
import { reserveClusterDomain } from "#/modules/cluster-domain/cluster-domain.server";
import { expandManagedHostnames } from "#/modules/environment-design/managed-hostnames.server";
import { OrganizationRuntime } from "#/modules/runtime/organization-runtime.server";
import { DeploymentExecutionError } from "./execution-error";
import { compileSdkPreparationInput } from "./runtime-preview";
import { loadResolvedDeployEnv, needsClusterDomain, type DeploymentContext } from "./runtime-repository.server";

/** What a deploy and an Image Build both need from the Organization's runtime: a session and the compiled target. */

export class DeploymentRuntimeUnavailable extends Data.TaggedError(
  "DeploymentRuntimeUnavailable",
)<{
  readonly failureCode: "runtime_not_connected" | "runtime_unreachable";
  readonly message: string;
}> {
  get retriable() {
    return this.failureCode !== "runtime_not_connected";
  }
}

export class DeploymentRuntimeInvalid extends Data.TaggedError(
  "DeploymentRuntimeInvalid",
)<{
  readonly failureCode:
    | "sdk_preview_invalid"
    | "sdk_outcome_invalid";
  readonly message: string;
  readonly cause?: unknown;
}> {
  readonly retriable = false as const;
}

/** The Organization's Cluster Domain name. The first deploy that needs one reserves it; a Hosted DNS failure refuses the deploy. */
const requireClusterDomain = (organizationId: string) =>
  reserveClusterDomain(organizationId).pipe(
    Effect.map((row) => row.name),
    Effect.catchTag("HostedDnsError", (cause) => Effect.fail(new DeploymentExecutionError({
      failureCode: "cluster_domain_unreserved",
      message: "Hosted DNS couldn’t reserve the Organization’s domain. Deploy again shortly.",
      cause,
    }))),
  );

/** The attempt's whole frozen target; an Image Build compiles the same so its fingerprint matches deploy's. */
export function compileRuntimeIntent(context: DeploymentContext) {
  return Effect.gen(function* () {
    const clusterDomain = needsClusterDomain(context) ? yield* requireClusterDomain(context.organization.id) : null;
    const resolvedEnv = yield* loadResolvedDeployEnv(context, clusterDomain);
    // requireClusterDomain already refused a deploy with managed hostnames and no Cluster Domain.
    const snapshots = context.snapshots.map((snapshot) => ({
      ...snapshot,
      config: clusterDomain === null ? snapshot.config : expandManagedHostnames(snapshot.config, clusterDomain),
      resolvedEnv: resolvedEnv.get(snapshot.serviceId),
    }));
    return yield* Effect.try({
      try: () =>
        compileSdkPreparationInput({
          projectName: context.environment.namespace,
          snapshots,
          volumes: context.volumes,
          variableProducers: context.deployment.variableProducers ?? [],
        }),
      catch: (cause) => {
        return new DeploymentRuntimeInvalid({
          failureCode: "sdk_preview_invalid",
          message: cause instanceof Error ? cause.message : "The immutable deployment target could not be compiled.",
          cause,
        });
      },
    });
  });
}

/** A session to the Organization's runtime; with `machineId`, through that entry Machine only. */
export function connectedRuntime(organizationId: string, machineId?: MachineId) {
  return Effect.gen(function* () {
    const runtime = yield* OrganizationRuntime;
    const session = yield* runtime.open(organizationId, machineId);
    switch (session.status) {
      case "connected":
        return session.connected;
      case "no_connection":
        return yield* new DeploymentRuntimeUnavailable({
          failureCode: "runtime_not_connected",
          message: "The Organization has no connected runtime.",
        });
      case "unreachable":
        return yield* new DeploymentRuntimeUnavailable({
          failureCode: "runtime_unreachable",
          message: "The Organization runtime is unreachable.",
        });
      default: {
        const exhaustive: never = session;
        return exhaustive;
      }
    }
  });
}

/** The one-Service target an Image Build gets: the whole frozen target narrowed to it, so its fingerprint matches deploy's. */
export const oneServiceDeployment = (context: DeploymentContext, serviceId: string) => compileRuntimeIntent(context).pipe(
  // Ordering between Services is deploy's concern.
  Effect.map((intent) => ({ ...intent, snapshots: intent.snapshots.filter((candidate) => candidate.serviceId === serviceId), dependencies: {} })),
);

/** Poll failure is fatal: a quiet operation must never outlive its cancellation observer. */
export function watchDeploymentCancellation<E, R>(
  stillWanted: Effect.Effect<boolean, E, R>,
  cancellation: AbortController,
) {
  return Effect.gen(function* () {
    while (true) {
      if (!(yield* stillWanted)) {
        cancellation.abort();
        return yield* Effect.never;
      }
      yield* Effect.sleep("1 second");
    }
  }).pipe(
    Effect.tapError(() => Effect.sync(() => cancellation.abort())),
    Effect.mapError((cause) => new DeploymentExecutionError({
      failureCode: "sdk_deploy_outcome_unknown", message: "Cancellation monitoring failed; remote execution outcome is unknown.", cause,
    })),
  );
}
