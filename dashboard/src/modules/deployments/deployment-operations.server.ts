import "@tanstack/react-start/server-only";

import { Effect, Schema } from "effect";

import { loadClusterDomain } from "#/modules/cluster-domain/cluster-domain.server";
import type {
  DeploymentBuildTailQueryInput,
  DeploymentOperationEvidencePageQueryInput,
  DeploymentServiceVariablesQueryInput,
  EnvironmentChangeStateNodeProjection,
  EnvironmentChangeStateProjection,
  OrganizationEnvironmentChangeStateQueryInput,
} from "#/modules/deployments/deployment-contract";
import { loadEnvironmentSnapshotProjection, type EnvironmentSnapshotProjection } from "#/modules/deployments/environment-state.repository.server";
import { decodeEnvironmentResourceNodeConfig } from "#/modules/environment-design/environment-resource-node";
import { strictParseOptions } from "#/modules/environment-design/schema";
import { withoutSealedCiphertext } from "#/modules/environment-design/saved-intent";
import { serviceDeploymentConfigSchema } from "#/modules/environment-design/services";
import { getOrganizationForUserBySlug } from "#/modules/environment-design/workspace-repository.server";
import type { Actor } from "#/modules/identity/actor";
import { Conflict, NotFound, Validation } from "#/server/public-error";

import { loadDeploymentBuildLog, loadDeploymentEvents } from "./deployment-events.server";
import { loadDeploymentContext, loadDisplayedDeployEnv, needsClusterDomain } from "./runtime-hydration.repository.server";

const requireOrganization = Effect.fn("Deployments.requireOrganization")(
  function* (actor: Actor, organizationSlug: string) {
    const organization = yield* getOrganizationForUserBySlug(
      actor.userId,
      organizationSlug,
    );
    if (organization !== null) return organization;
    return yield* new NotFound({
      message: "The organization was not found.",
    });
  },
);

function parseEnvironmentChangeStateNode(input: {
  readonly nodeType: "service" | "volume";
  readonly nodeId: string;
  readonly nodeLineageId: string;
  readonly revisionId: string | null;
  readonly config: unknown;
}) {
  const identity = {
    nodeId: input.nodeId,
    nodeLineageId: input.nodeLineageId,
    revisionId: input.revisionId,
  };
  const invalid = () =>
    new Conflict({ message: "Environment change state is invalid." });
  if (input.nodeType === "service") {
    // Sealed variable ciphertext stays on the server; the browser sees each sealed value's fingerprint.
    return Schema.decodeUnknownEffect(serviceDeploymentConfigSchema)(
      withoutSealedCiphertext(input.config),
      strictParseOptions,
    ).pipe(
      Effect.map(
        (config): EnvironmentChangeStateNodeProjection => ({
          ...identity,
          nodeType: "service",
          config,
        }),
      ),
      Effect.mapError(invalid),
    );
  }
  const nodeType = input.nodeType;
  return decodeEnvironmentResourceNodeConfig(nodeType, input.config).pipe(
    Effect.map((decoded) => ({
      ...identity,
      ...decoded,
    })),
    Effect.mapError(invalid),
  );
}

type EnvironmentChangeStateEvidenceNode = NonNullable<
  EnvironmentChangeStateProjection["deploymentEvidence"]
>["nodes"][number];

function parseEvidenceChangeStateNode(input: {
  readonly nodeType: "service" | "volume";
  readonly nodeId: string;
  readonly nodeLineageId: string;
  readonly revisionId: string | null;
  readonly config: unknown;
}): Effect.Effect<EnvironmentChangeStateEvidenceNode, Conflict> {
  return input.config === null
    ? Effect.succeed({ ...input, config: null })
    : parseEnvironmentChangeStateNode(input);
}

const projectEnvironmentChangeStateRecords = Effect.fn(
  "Deployments.projectEnvironmentChangeStateRecords",
)(function* (projection: EnvironmentSnapshotProjection) {
  return yield* Effect.forEach(projection.explicitStates, (state) =>
    Effect.gen(function* () {
      const savedNodes = yield* Effect.forEach(
        state.saved?.nodes ?? [],
        parseEnvironmentChangeStateNode,
      );
      const appliedNodes = yield* Effect.forEach(
        state.applied.nodes,
        parseEnvironmentChangeStateNode,
      );
      const evidenceNodes = yield* Effect.forEach(
        state.deploymentEvidence?.nodes ?? [],
        parseEvidenceChangeStateNode,
      );
      return {
        environmentId: state.environmentId,
        saved: state.saved ? { ...state.saved, nodes: savedNodes } : null,
        applied: { ...state.applied, nodes: appliedNodes },
        deploymentEvidence: state.deploymentEvidence
          ? { ...state.deploymentEvidence, nodes: evidenceNodes }
          : null,
      };
    }),
  );
});

export const listLatestOrganizationEnvironmentChangeStates = Effect.fn(
  "Deployments.listLatestOrganizationEnvironmentChangeStates",
)(function* (
  actor: Actor,
  input: OrganizationEnvironmentChangeStateQueryInput,
) {
  const organization = yield* requireOrganization(actor, input.organizationSlug);
  const projection = yield* loadEnvironmentSnapshotProjection({
      kind: "organization",
      organizationId: organization.id,
    });
  return yield* projectEnvironmentChangeStateRecords(projection);
});

const logCursor = Effect.fn("Deployments.logCursor")(function* (actor: Actor, input: DeploymentOperationEvidencePageQueryInput) {
  const organization = yield* requireOrganization(actor, input.organizationSlug);
  const after = Number(input.afterSequence ?? 0);
  if (!Number.isSafeInteger(after) || after < 0) return yield* new Validation({ message: "Invalid log cursor." });
  return { organizationId: organization.id, deploymentId: input.deploymentId, after };
});

export const listDeploymentBuildLog = Effect.fn("Deployments.buildLog")(function* (actor: Actor, input: DeploymentOperationEvidencePageQueryInput) {
  return yield* loadDeploymentBuildLog({ ...yield* logCursor(actor, input), limit: input.limit ?? 100 });
});

/** Each Build Step's last rows: enough for a node's log tail, however long the build ran. */
export const listDeploymentBuildTail = Effect.fn("Deployments.buildTail")(function* (actor: Actor, input: DeploymentBuildTailQueryInput) {
  return yield* loadDeploymentBuildLog({ ...yield* logCursor(actor, input), limit: 0, tail: 20 });
});

export const listDeploymentProgressLogs = Effect.fn("Deployments.progressLogs")(function* (actor: Actor, input: DeploymentOperationEvidencePageQueryInput) {
  return yield* loadDeploymentEvents(yield* logCursor(actor, input));
});

/**
 * One service's variables as the attempt deployed them, recomputed through deploy's own loader
 * from the attempt's frozen snapshots and producers. Sealed values, and values resolving from them,
 * are null: never decrypted.
 */
export const getDeploymentServiceVariables = Effect.fn("Deployments.serviceVariables")(function* (
  actor: Actor,
  input: DeploymentServiceVariablesQueryInput,
) {
  const organization = yield* requireOrganization(actor, input.organizationSlug);
  const context = yield* loadDeploymentContext(input.deploymentId);
  if (context?.organization.id !== organization.id) {
    return yield* new NotFound({ message: "The deployment was not found." });
  }
  // Deploy reserves the Cluster Domain; a read only looks. Once reserved it never changes.
  const clusterDomain = needsClusterDomain(context) ? (yield* loadClusterDomain(organization.id))?.name ?? null : null;
  const env = yield* loadDisplayedDeployEnv(context, clusterDomain);
  return env.get(input.serviceId) ?? {};
});
