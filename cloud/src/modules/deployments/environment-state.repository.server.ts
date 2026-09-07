import "@tanstack/react-start/server-only";
import { Effect, Option } from "effect";
import { and, asc, desc, eq, inArray } from "drizzle-orm";
import {
  environmentDeployment as schemaEnvironmentDeployment,
  environmentSavedStateSnapshot as schemaEnvironmentSavedStateSnapshot,
  environmentDeploymentSecret as schemaEnvironmentDeploymentSecret,
} from "#/modules/deployments/tables";
import {
  coreOperationWatch as schemaCoreOperationWatch,
  coreOperationEvent as schemaCoreOperationEvent,
} from "#/modules/operations/tables";
import {
  environment as schemaEnvironment,
  project as schemaProject,
} from "#/modules/project/tables";
import {
  environmentNodeConfigSnapshot as schemaEnvironmentNodeConfigSnapshot,
  environmentNodeConfigSnapshotSecret as schemaEnvironmentNodeConfigSnapshotSecret,
} from "#/modules/runtime/tables";
import { Database } from "#/server/database.server";
import { SecretEncryption } from "#/utils/encrypted-secret.server";
import type { EncryptedSecretValue, JsonObject } from "#/db/tables";
import type {
  EnvironmentDeploymentPreview,
  EnvironmentDeploymentStatus,
} from "#/modules/deployments/tables";
import { asRecord, asString } from "#/lib/json";
import { foldPhaseAwareAppliedState } from "#/modules/runtime/phase-aware-applied-state";
import {
  toPhaseAwareCurrentRuntimeServiceResult,
} from "#/modules/runtime/phase-aware-deploy-current-runtime-adapter";
import { decodeFrozenDeployInput } from "#/modules/deployments/frozen-input.server";
import {
  parsePhaseAwareDeployResultFromPhaseEvidence,
} from "#/modules/runtime/phase-aware-deploy-contract";
import { decodePersistedSavedEnvironmentState } from "#/modules/environment-design/saved-intent";
import { decodeRuntimePhaseEvidence } from "#/modules/operations/deploy-operation-evidence";

const ACTIVE_DEPLOYMENT_STATUSES = [
  "queued",
  "planning",
  "deploying",
] satisfies EnvironmentDeploymentStatus[];

type SnapshotScope =
  | { kind: "environment"; environmentId: string }
  | { kind: "organization"; organizationId: string };

export type EnvironmentExplicitStateProjectionNode = {
  nodeType: "service" | "variable_group" | "volume";
  nodeId: string;
  nodeLineageId: string;
  config: unknown;
  revisionId: string | null;
};

export type EnvironmentExplicitStateProjection = {
  environmentId: string;
  saved: {
    token: string;
    snapshotId: string;
    createdAt: Date;
    nodes: EnvironmentExplicitStateProjectionNode[];
  } | null;
  applied: {
    token: string;
    nodes: EnvironmentExplicitStateProjectionNode[];
  };
  deploymentEvidence: {
    id: string;
    status: EnvironmentDeploymentStatus;
    token: string;
    createdAt: Date;
    nodes: EnvironmentExplicitStateProjectionNode[];
  } | null;
};

export type EnvironmentSnapshotProjection = {
  appliedSavedNodeByKey: Map<
    string,
    {
      nodeType: "service" | "variable_group" | "volume";
      nodeId: string;
      nodeLineageId: string;
      configVersion: number;
      config: JsonObject;
      encryptedRegistryUsername: EncryptedSecretValue | null;
      encryptedRegistrySecret: EncryptedSecretValue | null;
      sourceSavedStateSnapshotId: string;
    }
  >;
  explicitStates: EnvironmentExplicitStateProjection[];
};

type LoadedNode = {
  environmentDeploymentId: string;
  environmentId: string;
  nodeType: "service" | "variable_group" | "volume";
  nodeId: string;
  nodeLineageId: string;
  configVersion: number;
  config: unknown;
  encryptedRegistryUsername: EncryptedSecretValue | null;
  encryptedRegistrySecret: EncryptedSecretValue | null;
  snapshotCreatedAt: Date;
};

type PartialPhaseEvent = {
  operationId: string;
  sequence: string;
  eventType: string;
  payload: JsonObject;
};

type PartialFrozenInput = {
  environmentDeploymentId: string;
  encryptedFrozenDeployInput: EncryptedSecretValue;
};

function nodeKey(nodeType: string, nodeId: string) {
  return `${nodeType}:${nodeId}`;
}

function requiredMapValue<K, V>(map: Map<K, V>, key: K, message: string): V {
  const value = map.get(key);
  if (value === undefined) throw new Error(message);
  return value;
}

function requiredJsonObject<T>(value: T, message: string): JsonObject {
  const record = asRecord(value);
  if (!record) throw new Error(message);
  return record;
}

const DEPLOYMENT_HEAD_COLUMNS = {
  environmentId: schemaEnvironmentDeployment.environmentId,
  id: schemaEnvironmentDeployment.id,
  savedStateSnapshotId: schemaEnvironmentDeployment.savedStateSnapshotId,
  status: schemaEnvironmentDeployment.status,
  createdAt: schemaEnvironmentDeployment.createdAt,
  coreDeployId: schemaEnvironmentDeployment.coreDeployId,
  deployPreview: schemaEnvironmentDeployment.deployPreview,
};

function loadDeploymentHeads(
  scope: SnapshotScope,
  input: {
    readonly statuses: readonly EnvironmentDeploymentStatus[];
    readonly selection: "latest" | "history";
  },
) {
  return Effect.gen(function* () {
    const { drizzle } = yield* Database;
    const statusFilter = inArray(schemaEnvironmentDeployment.status, [
      ...input.statuses,
    ]);
    if (scope.kind === "environment") {
      const query = drizzle
        .select(DEPLOYMENT_HEAD_COLUMNS)
        .from(schemaEnvironmentDeployment)
        .where(
          and(
            eq(schemaEnvironmentDeployment.environmentId, scope.environmentId),
            statusFilter,
          ),
        );
      if (input.selection === "latest") {
        return yield* query
          .orderBy(
            desc(schemaEnvironmentDeployment.createdAt),
            desc(schemaEnvironmentDeployment.id),
          )
          .limit(1);
      }
      return yield* query.orderBy(asc(schemaEnvironmentDeployment.createdAt));
    }
    if (input.selection === "latest") {
      return yield* drizzle
        .selectDistinctOn(
          [schemaEnvironmentDeployment.environmentId],
          DEPLOYMENT_HEAD_COLUMNS,
        )
        .from(schemaEnvironmentDeployment)
        .innerJoin(
          schemaEnvironment,
          eq(schemaEnvironmentDeployment.environmentId, schemaEnvironment.id),
        )
        .innerJoin(
          schemaProject,
          eq(schemaEnvironment.projectId, schemaProject.id),
        )
        .where(
          and(
            eq(schemaProject.organizationId, scope.organizationId),
            statusFilter,
          ),
        )
        .orderBy(
          asc(schemaEnvironmentDeployment.environmentId),
          desc(schemaEnvironmentDeployment.createdAt),
          desc(schemaEnvironmentDeployment.id),
        );
    }
    return yield* drizzle
      .select(DEPLOYMENT_HEAD_COLUMNS)
      .from(schemaEnvironmentDeployment)
      .innerJoin(
        schemaEnvironment,
        eq(schemaEnvironmentDeployment.environmentId, schemaEnvironment.id),
      )
      .innerJoin(
        schemaProject,
        eq(schemaEnvironment.projectId, schemaProject.id),
      )
      .where(
        and(
          eq(schemaProject.organizationId, scope.organizationId),
          statusFilter,
        ),
      )
      .orderBy(
        asc(schemaEnvironmentDeployment.environmentId),
        asc(schemaEnvironmentDeployment.createdAt),
      );
  });
}

function loadActiveDeploymentHeads(scope: SnapshotScope) {
  return loadDeploymentHeads(scope, {
    statuses: ACTIVE_DEPLOYMENT_STATUSES,
    selection: "latest",
  });
}

function loadAppliedDeploymentHeads(scope: SnapshotScope) {
  return loadDeploymentHeads(scope, {
    statuses: ["applied"],
    selection: "latest",
  });
}

function loadPartialDeploymentHeads(scope: SnapshotScope) {
  return loadDeploymentHeads(scope, {
    statuses: ["failed", "cancelled"],
    selection: "history",
  });
}

function loadSavedHeads(scope: SnapshotScope) {
  return Effect.gen(function* () {
    const { drizzle } = yield* Database;
    const columns = {
      environmentId: schemaEnvironmentSavedStateSnapshot.environmentId,
      id: schemaEnvironmentSavedStateSnapshot.id,
      intent: schemaEnvironmentSavedStateSnapshot.intent,
      createdAt: schemaEnvironmentSavedStateSnapshot.createdAt,
    };
    if (scope.kind === "environment") {
      return yield* drizzle
        .select(columns)
        .from(schemaEnvironmentSavedStateSnapshot)
        .where(
          eq(
            schemaEnvironmentSavedStateSnapshot.environmentId,
            scope.environmentId,
          ),
        )
        .orderBy(
          desc(schemaEnvironmentSavedStateSnapshot.createdAt),
          desc(schemaEnvironmentSavedStateSnapshot.id),
        )
        .limit(1);
    }
    return yield* drizzle
      .selectDistinctOn(
        [schemaEnvironmentSavedStateSnapshot.environmentId],
        columns,
      )
      .from(schemaEnvironmentSavedStateSnapshot)
      .innerJoin(
        schemaEnvironment,
        eq(
          schemaEnvironmentSavedStateSnapshot.environmentId,
          schemaEnvironment.id,
        ),
      )
      .innerJoin(
        schemaProject,
        eq(schemaEnvironment.projectId, schemaProject.id),
      )
      .where(eq(schemaProject.organizationId, scope.organizationId))
      .orderBy(
        asc(schemaEnvironmentSavedStateSnapshot.environmentId),
        desc(schemaEnvironmentSavedStateSnapshot.createdAt),
        desc(schemaEnvironmentSavedStateSnapshot.id),
      );
  });
}

function loadNodeConfigSnapshots(ids: readonly string[]) {
  return Effect.gen(function* () {
    if (ids.length === 0) return [];
    const { drizzle } = yield* Database;
    return yield* drizzle
      .select({
        environmentDeploymentId:
          schemaEnvironmentNodeConfigSnapshot.environmentDeploymentId,
        environmentId: schemaEnvironmentNodeConfigSnapshot.environmentId,
        nodeType: schemaEnvironmentNodeConfigSnapshot.nodeType,
        nodeId: schemaEnvironmentNodeConfigSnapshot.nodeId,
        nodeLineageId: schemaEnvironmentNodeConfigSnapshot.nodeLineageId,
        configVersion: schemaEnvironmentNodeConfigSnapshot.configVersion,
        config: schemaEnvironmentNodeConfigSnapshot.config,
        encryptedRegistryUsername:
          schemaEnvironmentNodeConfigSnapshotSecret.encryptedRegistryUsername,
        encryptedRegistrySecret:
          schemaEnvironmentNodeConfigSnapshotSecret.encryptedRegistrySecret,
        snapshotCreatedAt: schemaEnvironmentNodeConfigSnapshot.createdAt,
      })
      .from(schemaEnvironmentNodeConfigSnapshot)
      .leftJoin(
        schemaEnvironmentNodeConfigSnapshotSecret,
        eq(
          schemaEnvironmentNodeConfigSnapshotSecret.snapshotId,
          schemaEnvironmentNodeConfigSnapshot.id,
        ),
      )
      .where(
        inArray(
          schemaEnvironmentNodeConfigSnapshot.environmentDeploymentId,
          [...ids],
        ),
      );
  });
}

function projectSnapshotHeads(scope: SnapshotScope) {
  return Effect.gen(function* () {
    const { drizzle } = yield* Database;
    const encryption = yield* SecretEncryption;
  const [activeDeploymentHeads, savedHeads, appliedHeads, partialHeads] =
    yield* Effect.all([
      loadActiveDeploymentHeads(scope),
      loadSavedHeads(scope),
      loadAppliedDeploymentHeads(scope),
      loadPartialDeploymentHeads(scope),
    ]);
  const activeDeploymentByEnvironment = new Map(
    activeDeploymentHeads.map((head) => [
      head.environmentId,
      head,
    ]),
  );
  const savedByEnvironment = new Map(
    savedHeads.map((head) => [head.environmentId, head]),
  );
  const deploymentHeadById = new Map(
    activeDeploymentHeads.map((head) => [head.id, head]),
  );
  const savedStateSnapshotIdByDeploymentId = new Map(
    [
      ...activeDeploymentHeads,
      ...appliedHeads,
      ...partialHeads,
    ].map((head) => [head.id, head.savedStateSnapshotId] as const),
  );
  const deploymentIds = [...deploymentHeadById.keys()];
  const appliedIds = appliedHeads.map((head) => head.id);
  const partialIds = partialHeads.map((head) => head.id);
  const liveCandidateIds = [...new Set([...appliedIds, ...partialIds])];
  const partialOperationIds = partialHeads.flatMap((head) =>
    head.coreDeployId ? [head.coreDeployId] : [],
  );
  const [
    deploymentNodes,
    liveCandidateNodes,
    partialEvents,
    partialFrozenInputs,
  ] =
    yield* Effect.all([
      loadNodeConfigSnapshots(deploymentIds),
      loadNodeConfigSnapshots(liveCandidateIds),
      partialOperationIds.length === 0
        ? Effect.succeed([])
        : drizzle
            .select({
              operationId: schemaCoreOperationWatch.operationId,
              sequence: schemaCoreOperationEvent.sequence,
              eventType: schemaCoreOperationEvent.eventType,
              payload: schemaCoreOperationEvent.payload,
            })
            .from(schemaCoreOperationEvent)
            .innerJoin(
              schemaCoreOperationWatch,
              eq(
                schemaCoreOperationEvent.watchId,
                schemaCoreOperationWatch.id,
              ),
            )
            .where(
              and(
                inArray(
                  schemaCoreOperationWatch.operationId,
                  partialOperationIds,
                ),
                eq(
                  schemaCoreOperationEvent.eventType,
                  "deploy_phase_finished",
                ),
              ),
            ),
      partialIds.length === 0
        ? Effect.succeed<PartialFrozenInput[]>([])
        : drizzle
            .select({
              environmentDeploymentId:
                schemaEnvironmentDeploymentSecret.environmentDeploymentId,
              encryptedFrozenDeployInput:
                schemaEnvironmentDeploymentSecret.encryptedFrozenDeployInput,
            })
            .from(schemaEnvironmentDeploymentSecret)
            .where(
              inArray(
                schemaEnvironmentDeploymentSecret.environmentDeploymentId,
                partialIds,
              ),
            ),
    ]);

  const candidateNodesByDeploymentId = new Map<string, LoadedNode[]>();
  for (const node of liveCandidateNodes) {
    const nodes = candidateNodesByDeploymentId.get(
      node.environmentDeploymentId,
    );
    if (nodes) nodes.push(node);
    else candidateNodesByDeploymentId.set(node.environmentDeploymentId, [node]);
  }

  const previewRevision = (
    preview: EnvironmentDeploymentPreview | null,
    runtimeServiceId: string,
  ) => {
    const commits = asRecord(asRecord(preview)?.["projection"])?.[
      "serving_target_commits"
    ];
    if (!Array.isArray(commits)) return null;
    for (const entry of commits) {
      const record = asRecord(entry);
      if (asString(record?.["service_id"]) === runtimeServiceId) {
        return asString(record?.["namespace_revision_entry_id"]) ?? null;
      }
    }
    return null;
  };
  const runtimeServiceId = (node: LoadedNode) => {
    if (node.nodeType !== "service") return null;
    const privateDns = asString(asRecord(node.config)?.["privateDns"]);
    return privateDns;
  };

  const liveNodesByKey = new Map<string, LoadedNode>();
  const liveRevisionByNodeKey = new Map<string, string | null>();
  const appliedCreatedAtByEnvironment = new Map<string, Date>();
  for (const head of appliedHeads) {
    appliedCreatedAtByEnvironment.set(head.environmentId, head.createdAt);
    for (const node of candidateNodesByDeploymentId.get(head.id) ?? []) {
      const key = nodeKey(node.nodeType, node.nodeId);
      liveNodesByKey.set(key, node);
      const serviceId = runtimeServiceId(node);
      liveRevisionByNodeKey.set(
        key,
        serviceId ? previewRevision(head.deployPreview, serviceId) : null,
      );
    }
  }

  const eventsByOperationId = new Map<string, PartialPhaseEvent[]>();
  for (const event of partialEvents) {
    const events = eventsByOperationId.get(event.operationId);
    if (events) events.push(event);
    else eventsByOperationId.set(event.operationId, [event]);
  }
  const frozenByDeploymentId = new Map(
    partialFrozenInputs.map((frozen) => [
      frozen.environmentDeploymentId,
      frozen.encryptedFrozenDeployInput,
    ]),
  );
  for (const head of partialHeads) {
    const appliedAt = appliedCreatedAtByEnvironment.get(head.environmentId);
    if (appliedAt && head.createdAt <= appliedAt) continue;
    const nodes = candidateNodesByDeploymentId.get(head.id) ?? [];
    const target = nodes.flatMap((node) => {
      const serviceId = runtimeServiceId(node);
      return serviceId ? [{ serviceId, node }] : [];
    });
    const targetByRuntimeServiceId = new Map(
      target.map((binding) => [binding.serviceId, binding] as const),
    );
    const prior = [...liveNodesByKey.values()].flatMap((node) => {
      if (node.environmentId !== head.environmentId) return [];
      const serviceId = runtimeServiceId(node);
      return serviceId ? [{ serviceId, node }] : [];
    });
    const priorRevisionByNodeKey = new Map(
      prior.map(({ node }) => [
        nodeKey(node.nodeType, node.nodeId),
        liveRevisionByNodeKey.get(nodeKey(node.nodeType, node.nodeId)) ?? null,
      ]),
    );
    const frozenSecret = frozenByDeploymentId.get(head.id);
    if (!frozenSecret) continue;
    const frozen = yield* decodeFrozenDeployInput(
      encryption,
      frozenSecret,
    ).pipe(Effect.option);
    if (Option.isNone(frozen) || !frozen.value) continue;
    const phases = [];
    const events = [
      ...(eventsByOperationId.get(head.coreDeployId ?? "") ?? []),
    ].sort((left, right) => {
      const leftSequence = BigInt(left.sequence);
      const rightSequence = BigInt(right.sequence);
      return leftSequence === rightSequence
        ? 0
        : leftSequence < rightSequence
          ? -1
          : 1;
    });
    for (const event of events) {
      const decoded = yield* decodeRuntimePhaseEvidence({
          eventType: event.eventType,
          payload: event.payload,
        }).pipe(Effect.option);
      if (Option.isSome(decoded)) {
        phases.push({
          phase: decoded.value.payload.phase,
          services: decoded.value.payload.services.map(
            toPhaseAwareCurrentRuntimeServiceResult,
          ),
        });
      }
    }
    const validated = yield* parsePhaseAwareDeployResultFromPhaseEvidence(
        frozen.value.request,
        phases,
      ).pipe(Effect.option);
    if (Option.isNone(validated)) continue;
    const folded = foldPhaseAwareAppliedState({
      prior,
      target,
      result: validated.value,
    });
    for (const { node } of prior) {
      const key = nodeKey(node.nodeType, node.nodeId);
      liveNodesByKey.delete(key);
      liveRevisionByNodeKey.delete(key);
    }
    for (const binding of folded) {
      const { node } = binding;
      const key = nodeKey(node.nodeType, node.nodeId);
      liveNodesByKey.set(key, node);
      liveRevisionByNodeKey.set(
        key,
        targetByRuntimeServiceId.get(binding.serviceId)?.node === node
          ? previewRevision(head.deployPreview, binding.serviceId)
          : (priorRevisionByNodeKey.get(key) ?? null),
      );
    }
  }

  const environmentIds = [
    ...new Set([
      ...savedByEnvironment.keys(),
      ...activeDeploymentByEnvironment.keys(),
      ...[...liveNodesByKey.values()].map((node) => node.environmentId),
    ]),
  ].sort((left, right) => left.localeCompare(right));
  const explicitStates = yield* Effect.forEach(
    environmentIds,
    (environmentId) =>
      Effect.gen(function* () {
      const saved = savedByEnvironment.get(environmentId) ?? null;
      let savedNodes: EnvironmentExplicitStateProjectionNode[] | null = null;
      if (saved) {
        const decodedSaved = yield* decodePersistedSavedEnvironmentState(saved);
        savedNodes = decodedSaved.nodeSnapshots.map((node) => ({
          nodeType: node.nodeType,
          nodeId: node.nodeId,
          nodeLineageId: node.nodeLineageId,
          config: node.config,
          revisionId: null,
        }));
      }
      const activeDeployment =
        activeDeploymentByEnvironment.get(environmentId) ?? null;
      const appliedEntries = [...liveNodesByKey.entries()]
        .filter(([, node]) => node.environmentId === environmentId)
        .sort(([left], [right]) => left.localeCompare(right));
      const appliedTokenParts = appliedEntries.map(
        ([key, node]) => `${key}:${node.environmentDeploymentId}`,
      );
      const targetNodes = activeDeployment
        ? deploymentNodes.filter(
            (node) => node.environmentDeploymentId === activeDeployment.id,
          )
        : [];
      const targetKeys = new Set(
        targetNodes.map((node) => nodeKey(node.nodeType, node.nodeId)),
      );
      const evidenceNodes: EnvironmentExplicitStateProjectionNode[] = [
        ...targetNodes.map((node) => {
          const serviceId = runtimeServiceId(node);
          return {
            nodeType: node.nodeType,
            nodeId: node.nodeId,
            nodeLineageId: node.nodeLineageId,
            config: node.config,
            revisionId:
              serviceId && activeDeployment
                ? previewRevision(activeDeployment.deployPreview, serviceId)
                : null,
          };
        }),
        ...appliedEntries.flatMap(([key, node]) =>
          targetKeys.has(key)
            ? []
            : [
                {
                  nodeType: node.nodeType,
                  nodeId: node.nodeId,
                  nodeLineageId: node.nodeLineageId,
                  config: null,
                  revisionId: null,
                },
              ],
        ),
      ].sort((left, right) =>
        nodeKey(left.nodeType, left.nodeId).localeCompare(
          nodeKey(right.nodeType, right.nodeId),
        ),
      );

      return {
        environmentId,
        saved: saved
          ? {
              token: `saved:${saved.id}`,
              snapshotId: saved.id,
              createdAt: saved.createdAt,
              nodes: savedNodes ?? [],
            }
          : null,
        applied: {
          token:
            appliedTokenParts.length > 0
              ? `applied:${appliedTokenParts.join("|")}`
              : `applied:none:${environmentId}`,
          nodes: appliedEntries.map(([, node]) => ({
            nodeType: node.nodeType,
            nodeId: node.nodeId,
            nodeLineageId: node.nodeLineageId,
            config: node.config,
            revisionId:
              liveRevisionByNodeKey.get(nodeKey(node.nodeType, node.nodeId)) ??
              null,
          })),
        },
        deploymentEvidence: activeDeployment
          ? {
              id: activeDeployment.id,
              status: activeDeployment.status,
              token: `deployment:${activeDeployment.id}`,
              createdAt: activeDeployment.createdAt,
              nodes: evidenceNodes,
            }
          : null,
      };
      }),
  );

  return {
    appliedSavedNodeByKey: new Map(
      [...liveNodesByKey].map(([key, node]) => [
        key,
        {
          nodeType: node.nodeType,
          nodeId: node.nodeId,
          nodeLineageId: node.nodeLineageId,
          configVersion: node.configVersion,
          config: requiredJsonObject(
            node.config,
            "Applied node config is not a JSON object.",
          ),
          encryptedRegistryUsername: node.encryptedRegistryUsername,
          encryptedRegistrySecret: node.encryptedRegistrySecret,
          sourceSavedStateSnapshotId: requiredMapValue(
            savedStateSnapshotIdByDeploymentId,
            node.environmentDeploymentId,
            "Applied deployment is missing Saved provenance.",
          ),
        },
      ]),
    ),
    explicitStates,
  } satisfies EnvironmentSnapshotProjection;
  });
}

export function loadEnvironmentSnapshotProjection(scope: SnapshotScope) {
  return projectSnapshotHeads(scope);
}
