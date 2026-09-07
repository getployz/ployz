import type { JsonValue } from "#/db/tables";
import { asBoolean, asFiniteNumber, asRecord, asString } from "#/lib/json";
import type { VariableGroupResourceRecord, VolumeResourceRecord } from "./resources";
import { projectVariableGroupConfig } from "./variable-group-config";
import { namedVolumeConfig } from "./volume-config";
import type { EnvironmentServiceViewRecord } from "#/modules/services/services.collection";
import { projectServiceDeploymentConfig } from "./services";
import type { DestructiveVolumeReview } from "./destructive-volume-review";
import type { EnvironmentSavedStateBasis } from "./saved-state";

type ReviewedNodeSnapshot = {
  nodeType: "service" | "variable_group" | "volume";
  nodeId: string;
  nodeLineageId: string;
  configVersion: number;
  config: unknown;
};

export type ReviewableNodeIdentity = Pick<
  ReviewedNodeSnapshot,
  "nodeType" | "nodeId"
> & {
  config?: unknown | null;
};

export type DestructiveEnvironmentSave = {
  serviceIds: string[];
  volumeIds: string[];
};

/**
 * Authority to publish one exact Working State revision as Saved State.
 *
 * The destructive set is required even when empty so every publisher is
 * checked against the same locked transition instead of opting into safety.
 */
export type ReviewedEnvironmentPublication = {
  savedStateBasis: EnvironmentSavedStateBasis;
  workingStateFingerprint: string;
  destructiveServiceIds: string[];
  destructiveVolumeReviews: DestructiveVolumeReview[];
};

export function projectReviewedEnvironmentPublicationDestructiveSave(
  review: ReviewedEnvironmentPublication,
): DestructiveEnvironmentSave {
  return {
    serviceIds: review.destructiveServiceIds,
    volumeIds: review.destructiveVolumeReviews.map(
      (volumeReview) => volumeReview.target.resourceId,
    ),
  };
}

export function getDestructiveEnvironmentSaveReviewMismatch(input: {
  expected: DestructiveEnvironmentSave;
  reviewed: DestructiveEnvironmentSave;
}) {
  for (const nodeType of ["serviceIds", "volumeIds"] as const) {
    const expected = [...new Set(input.expected[nodeType])].sort(
      compareCodeUnits,
    );
    const reviewed = [...new Set(input.reviewed[nodeType])].sort(
      compareCodeUnits,
    );
    if (
      expected.length !== input.expected[nodeType].length ||
      reviewed.length !== input.reviewed[nodeType].length
    ) {
      return "A destructive Save review contains duplicate Environment Nodes.";
    }
    if (
      expected.length !== reviewed.length ||
      expected.some((nodeId, index) => nodeId !== reviewed[index])
    ) {
      return "The set of deployed Environment Nodes awaiting removal changed after review.";
    }
  }
  return null;
}

/** Deployed removals newly published by this Working-to-Saved transition. */
export function projectDestructiveEnvironmentSave(input: {
  workingNodes: readonly ReviewableNodeIdentity[];
  savedNodes: readonly ReviewableNodeIdentity[];
  appliedNodes: readonly ReviewableNodeIdentity[];
}): DestructiveEnvironmentSave {
  const working = new Set(
    input.workingNodes.flatMap((node) =>
      node.config === null ? [] : [`${node.nodeType}:${node.nodeId}`],
    ),
  );
  const applied = new Set(
    input.appliedNodes.flatMap((node) =>
      node.config === null ? [] : [`${node.nodeType}:${node.nodeId}`],
    ),
  );
  const removals = input.savedNodes.filter((node) => {
    const key = `${node.nodeType}:${node.nodeId}`;
    return node.config !== null && !working.has(key) && applied.has(key);
  });
  return {
    serviceIds: removals
      .filter((node) => node.nodeType === "service")
      .map((node) => node.nodeId)
      .sort(compareCodeUnits),
    volumeIds: removals
      .filter((node) => node.nodeType === "volume")
      .map((node) => node.nodeId)
      .sort(compareCodeUnits),
  };
}

export type ReviewedEnvironmentWorkingState = {
  nodeSnapshots: ReviewedNodeSnapshot[];
  revisionMarkers?: string[];
  tombstonedVolumeIds: string[];
};

function dateMarker(prefix: string, id: string, updatedAt: Date) {
  return `${prefix}:${id}:${updatedAt.toISOString()}`;
}

function compareCodeUnits(left: string, right: string) {
  return left < right ? -1 : left > right ? 1 : 0;
}

/** Project the exact persisted canvas revision that a manual action rendered. */
export function projectReviewedEnvironmentWorkingState(input: {
  services: EnvironmentServiceViewRecord[];
  variableGroups: VariableGroupResourceRecord[];
  volumes: VolumeResourceRecord[];
}): ReviewedEnvironmentWorkingState {
  const activeServices = input.services.filter(
    ({ service }) => service.deletedAt === null,
  );
  const activeVolumes = input.volumes.filter(
    ({ resource }) => resource.deletedAt === null,
  );
  return {
    nodeSnapshots: [
      ...activeServices.map(({ service }) => ({
        nodeType: "service" as const,
        nodeId: service.id,
        nodeLineageId: service.lineageId,
        configVersion: 1,
        config: projectServiceDeploymentConfig(service),
      })),
      ...input.variableGroups.map((resource) => ({
        nodeType: "variable_group" as const,
        nodeId: resource.resource.id,
        nodeLineageId: resource.resource.lineageId,
        configVersion: 1,
        config: projectVariableGroupConfig(resource),
      })),
      ...activeVolumes.map((resource) => ({
        nodeType: "volume" as const,
        nodeId: resource.resource.id,
        nodeLineageId: resource.resource.lineageId,
        configVersion: 2,
        config: namedVolumeConfig(resource.resource.name),
      })),
    ],
    revisionMarkers: [
      ...activeServices.map(({ service }) =>
        dateMarker("service", service.id, service.updatedAt),
      ),
      ...activeServices.flatMap(({ variables }) =>
        variables.map((variable) =>
          dateMarker("variable", variable.id, variable.updatedAt),
        ),
      ),
      ...input.variableGroups.flatMap((resource) => [
        dateMarker(
          "resource",
          resource.resource.id,
          resource.resource.updatedAt,
        ),
        dateMarker(
          "variable-group",
          resource.variableGroup.id,
          resource.variableGroup.updatedAt,
        ),
        ...resource.variables.map((variable) =>
          dateMarker("variable", variable.id, variable.updatedAt),
        ),
      ]),
      ...input.volumes.map((resource) =>
        dateMarker(
          "resource",
          resource.resource.id,
          resource.resource.updatedAt,
        ),
      ),
    ],
    tombstonedVolumeIds: input.volumes.flatMap(({ resource }) =>
      resource.deletedAt === null ? [] : [resource.id],
    ),
  };
}

type CanonicalValue = JsonValue;

// The canvas exposes secret fingerprints and resolved template values, not
// ciphertext or stored template parts. Revision markers still make changes to
// those backing rows stale even when their display value is unchanged.
function reviewable<T>(value: T): JsonValue {
  if (value === null) return null;
  const text = asString(value);
  if (text !== null) return text;
  const number = asFiniteNumber(value);
  if (number !== null) return number;
  const bool = asBoolean(value);
  if (bool !== null) return bool;
  if (Array.isArray(value)) return value.map(reviewable);
  const record = asRecord(value);
  if (record === null) {
    throw new TypeError("Reviewed Environment State must be JSON.");
  }
  return Object.fromEntries(
    Object.entries(record)
      .filter(
        ([key, item]) =>
          key !== "encryptedValue" && key !== "parts" && item !== undefined,
      )
      .sort(([left], [right]) => compareCodeUnits(left, right))
      .map(([key, item]) => [key, reviewable(item)]),
  );
}

function canonicalJson(value: CanonicalValue): string {
  return JSON.stringify(reviewable(value));
}

function canonicalReviewedNode(snapshot: ReviewedNodeSnapshot): JsonValue {
  return reviewable({
    nodeType: snapshot.nodeType,
    nodeId: snapshot.nodeId,
    nodeLineageId: snapshot.nodeLineageId,
    configVersion: snapshot.configVersion,
    config: snapshot.config,
  });
}

export function canonicalReviewedEnvironmentWorkingStateJson(
  input: ReviewedEnvironmentWorkingState,
): string {
  return canonicalJson({
    nodeSnapshots: [...input.nodeSnapshots]
      .sort((left, right) =>
        compareCodeUnits(
          `${left.nodeType}:${left.nodeId}`,
          `${right.nodeType}:${right.nodeId}`,
        ),
      )
      .map(canonicalReviewedNode),
    revisionMarkers: [...(input.revisionMarkers ?? [])].sort(compareCodeUnits),
    tombstonedVolumeIds: [...input.tombstonedVolumeIds].sort(compareCodeUnits),
  });
}

export function formatReviewedEnvironmentWorkingStateFingerprint(
  digestHex: string,
): string {
  return `environment-working-state-v1:${digestHex}`;
}

export async function fingerprintReviewedEnvironmentWorkingState(
  input: ReviewedEnvironmentWorkingState,
): Promise<string> {
  const digest = await globalThis.crypto.subtle.digest(
    "SHA-256",
    new TextEncoder().encode(canonicalReviewedEnvironmentWorkingStateJson(input)),
  );
  return formatReviewedEnvironmentWorkingStateFingerprint(
    Array.from(new Uint8Array(digest), (byte) =>
      byte.toString(16).padStart(2, "0"),
    ).join(""),
  );
}
