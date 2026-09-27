import "@tanstack/react-start/server-only";
import { and, eq, inArray, isNull } from "drizzle-orm";
import { Effect } from "effect";
import { liveValues, parseServiceConfig } from "@ployz/sdk/config";
import { Database } from "#/server/database.server";
import { environment, environmentBranch } from "#/modules/project/tables";
import { service, serviceLineage } from "#/modules/environment-design/tables";
import { environmentDeployment, type MissingLiveValue } from "#/modules/deployments/tables";
import type { SavedDeploymentTarget } from "#/modules/deployments/admission.server";
import { loadEnvironmentSnapshotProjection, type AppliedSavedNode } from "#/modules/deployments/environment-state.repository.server";
import { loadClusterDomain } from "#/modules/cluster-domain/cluster-domain.server";
import { servicePublicDomain } from "#/modules/environment-design/managed-service-exports";
import { ancestors } from "#/modules/project/environment-tree";
import { liveOwner } from "./live-owner";

type Producers = SavedDeploymentTarget["variableProducers"];

/**
 * A Branch's attempt as admission writes it, captured fresh on every attempt: plus each never-deployed Own Copy's Setup
 * Commands by service id, the values of the Live Nodes its Own Copies reference, and the ones no ancestor provides. A
 * root's comes back unchanged.
 */
export const branchAdmission = Effect.fn("Branches.branchAdmission")(function* (
  environmentId: string,
  target: SavedDeploymentTarget,
) {
  const live = yield* liveValuesOf(environmentId, target);
  return { ...target, ...live, setupCommands: yield* setupCommandsOf(environmentId) } satisfies SavedDeploymentTarget;
});

/**
 * A Branch's ancestors, nearest first, each with its namespace and, per Applied lineage, the attempt that applied that
 * node. Per node, so a node a partly failed attempt deployed counts. No ancestors for a root.
 */
export const loadAncestorApplied = Effect.fn("Branches.loadAncestorApplied")(function* (environmentId: string) {
  const { drizzle } = yield* Database;
  const [self] = yield* drizzle.select({
    organizationId: environmentBranch.organizationId, projectId: environmentBranch.projectId, parentId: environmentBranch.parentEnvironmentId,
  }).from(environmentBranch).where(eq(environmentBranch.environmentId, environmentId));
  if (!self) return { organizationId: null, parentId: null, branches: [], applied: new Map<string, AncestorApplied>() };
  const branches = yield* drizzle.select({ environmentId: environmentBranch.environmentId, parentEnvironmentId: environmentBranch.parentEnvironmentId })
    .from(environmentBranch).where(eq(environmentBranch.projectId, self.projectId));
  const ids = ancestors(self.parentId, branches);
  const namespaces = new Map((yield* drizzle.select({ id: environment.id, namespace: environment.namespace })
    .from(environment).where(inArray(environment.id, ids))).map((row) => [row.id, row.namespace]));
  const applied = new Map<string, AncestorApplied>();
  for (const id of ids) {
    const projection = yield* loadEnvironmentSnapshotProjection({ kind: "environment", environmentId: id });
    const nodes = [...projection.appliedSavedNodeByKey.values()].filter((node) => node.environmentId === id);
    applied.set(id, { namespace: namespaces.get(id) ?? "", lineages: new Map(nodes.map((node) => [node.nodeLineageId, node])) });
  }
  return { organizationId: self.organizationId, parentId: self.parentId, branches, applied };
});
type AncestorApplied = {
  namespace: string;
  /** Each Applied node by lineage, with the attempt that applied it. */
  lineages: Map<string, AppliedSavedNode>;
};

/** From the nearest ancestor that runs each Live Node its Own Copies reference, and the ones no ancestor provides. */
const liveValuesOf = Effect.fn("Branches.liveValuesOf")(function* (
  environmentId: string,
  target: SavedDeploymentTarget,
) {
  const { drizzle } = yield* Database;
  const variableProducers: Producers = [...target.variableProducers];
  const missingLiveValues: MissingLiveValue[] = [];
  // Live lineage → key read → the Own Copies that read it.
  const own = new Set(target.nodeSnapshots.map((node) => node.nodeLineageId));
  const uses = new Map<string, Map<string, Set<string>>>();
  for (const producer of target.variableProducers) {
    if (producer.value.kind !== "template") continue;
    for (const part of producer.value.parts) {
      if (part.kind !== "ref" || part.owner.scope !== "service" || own.has(part.owner.lineageId)) continue;
      const keys = uses.get(part.owner.lineageId) ?? new Map<string, Set<string>>();
      uses.set(part.owner.lineageId, keys.set(part.key, (keys.get(part.key) ?? new Set()).add(producer.ownerId)));
    }
  }
  if (uses.size === 0) return { variableProducers, missingLiveValues };

  // The owner of a Live lineage is the nearest ancestor whose Applied State runs it.
  const { organizationId, parentId, branches, applied } = yield* loadAncestorApplied(environmentId);
  const runs = new Map([...applied].map(([id, at]) => [id, at.lineages]));
  const byOwner = new Map<string, string[]>();
  const missing: { lineageId: string; key: string }[] = [];
  for (const lineageId of uses.keys()) {
    const owner = parentId ? liveOwner(parentId, lineageId, branches, runs) : null;
    if (owner) byOwner.set(owner, [...byOwner.get(owner) ?? [], lineageId]);
    else for (const key of uses.get(lineageId)?.keys() ?? []) missing.push({ lineageId, key });
  }
  const attemptIds = [...new Set([...byOwner.keys()].flatMap((owner) => [...applied.get(owner)?.lineages.values() ?? []]
    .map((node) => node.environmentDeploymentId)))];
  const attempts = attemptIds.length ? yield* drizzle.select({
    id: environmentDeployment.id, createdAt: environmentDeployment.createdAt, producers: environmentDeployment.variableProducers,
  }).from(environmentDeployment).where(inArray(environmentDeployment.id, attemptIds)) : [];
  const producersOf = new Map(attempts.map((row) => [row.id, row.producers]));
  const reads = (key: string) => [...uses.values()].some((keys) => keys.has(key));
  const clusterDomain = organizationId && reads("PLOYZ_PUBLIC_DOMAIN") ? (yield* loadClusterDomain(organizationId))?.name ?? null : null;
  for (const [owner, lineages] of byOwner) {
    const at = applied.get(owner);
    const nodes = [...at?.lineages ?? []];
    // Each node the owner runs, with its values from the attempt that applied it there (a Live Node's own values may
    // read the owner's other nodes); then what the owner itself uses live, as its newest of those attempts captured it.
    const newest = attempts.filter((row) => nodes.some(([, node]) => node.environmentDeploymentId === row.id))
      .reduce<(typeof attempts)[number] | undefined>((latest, row) => (latest && latest.createdAt > row.createdAt ? latest : row), undefined);
    const producers = [
      ...nodes.flatMap(([lineage, node]) => (producersOf.get(node.environmentDeploymentId) ?? [])
        .filter((producer) => producer.ownerLineageId === lineage)),
      ...(newest?.producers ?? []).filter((producer) => !at?.lineages.has(producer.ownerLineageId)),
      // A service's public address is resolved at deploy time from its own configuration, so the owner's attempt doesn't hold it.
      ...lineages.flatMap((lineage) => {
        const node = at?.lineages.get(lineage);
        const domain = node?.nodeType === "service" && uses.get(lineage)?.has("PLOYZ_PUBLIC_DOMAIN")
          ? servicePublicDomain(parseServiceConfig(node.config), clusterDomain) : null;
        return node && domain ? [{
          ownerScope: "service" as const, ownerId: node.nodeId, ownerLineageId: lineage, key: "PLOYZ_PUBLIC_DOMAIN",
          value: { kind: "literal" as const, value: domain },
        }] : [];
      }),
    ];
    const result = liveValues({
      owner: { namespace: at?.namespace ?? "", producers },
      lineages: lineages.map((lineageId) => ({ lineageId, keys: [...uses.get(lineageId)?.keys() ?? []] })),
    });
    variableProducers.push(...result.producers);
    missing.push(...result.missing);
  }
  if (missing.length === 0) return { variableProducers, missingLiveValues };

  const names = new Map((yield* drizzle.select({ id: serviceLineage.id, name: serviceLineage.canonicalName }).from(serviceLineage)
    .where(inArray(serviceLineage.id, missing.map((value) => value.lineageId)))).map((row) => [row.id, row.name]));
  for (const { lineageId, key } of missing) {
    for (const serviceId of uses.get(lineageId)?.get(key) ?? []) {
      missingLiveValues.push({ serviceId, from: names.get(lineageId) ?? lineageId, key });
    }
  }
  return { variableProducers, missingLiveValues };
});

/** Each never-deployed Own Copy's Setup Commands, by service id. */
const setupCommandsOf = Effect.fn("Branches.setupCommandsOf")(function* (environmentId: string) {
  const { drizzle } = yield* Database;
  const [branch] = yield* drizzle.select({ setupCommands: environmentBranch.setupCommands })
    .from(environmentBranch).where(eq(environmentBranch.environmentId, environmentId));
  const setupCommands: Record<string, string[]> = {};
  if (branch?.setupCommands.length) {
    const fresh = yield* drizzle.select({ id: service.id, lineageId: service.lineageId }).from(service)
      .where(and(eq(service.environmentId, environmentId), isNull(service.firstDeployedAt)));
    for (const { id, lineageId } of fresh) {
      const commands = branch.setupCommands.filter((setup) => setup.lineageId === lineageId).map((setup) => setup.command);
      if (commands.length) setupCommands[id] = commands;
    }
  }
  return setupCommands;
});
