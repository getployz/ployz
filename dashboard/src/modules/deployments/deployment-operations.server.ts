import { loadDeploymentEvents } from "./deployment-events.server";

import "@tanstack/react-start/server-only";

import { Effect, Schema } from "effect";

import { Conflict, NotFound, Validation } from "#/server/public-error";

import { loadAuthorizedDeploymentEvidence } from "#/modules/deployments/retry-repository.server";

import { loadEnvironmentSnapshotProjection, type EnvironmentSnapshotProjection } from "#/modules/deployments/environment-state.repository.server";

import type { Actor } from "#/modules/identity/actor";
import { serviceDeploymentConfigSchema } from "#/modules/environment-design/services";
import { decodeEnvironmentResourceNodeConfig } from "#/modules/environment-design/environment-resource-node";
import { strictParseOptions } from "#/modules/environment-design/schema";

import { getOrganizationForUserBySlug } from "#/modules/environment-design/workspace-repository.server";

import type { DeploymentOperationEvidencePageQueryInput, EnvironmentChangeStateNodeProjection, EnvironmentChangeStateProjection, OrganizationEnvironmentChangeStateQueryInput } from "#/modules/deployments/deployment-contract";

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
  readonly nodeType: "service" | "variable_group" | "volume";
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
    return Schema.decodeUnknownEffect(serviceDeploymentConfigSchema)(
      input.config,
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
  readonly nodeType: "service" | "variable_group" | "volume";
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

export const listDeploymentOperationEvidence = Effect.fn(
  "Deployments.listDeploymentOperationEvidence",
)(function* (actor: Actor, input: DeploymentOperationEvidencePageQueryInput) {
  const organization = yield* requireOrganization(actor, input.organizationSlug);
  return yield* loadAuthorizedDeploymentEvidence({
      organizationId: organization.id,
      deploymentId: input.deploymentId,
      afterSequence: input.afterSequence,
      limit: input.limit ?? 50,
    });
});

export const listDeploymentProgressLogs = Effect.fn("Deployments.progressLogs")(function* (actor: Actor, input: DeploymentOperationEvidencePageQueryInput) {
  const organization = yield* requireOrganization(actor, input.organizationSlug);
  const after = Number(input.afterSequence ?? 0);
  if (!Number.isSafeInteger(after) || after < 0) return yield* new Validation({ message: "Invalid log cursor." });
  return yield* loadDeploymentEvents({ organizationId: organization.id, deploymentId: input.deploymentId, after });
});
