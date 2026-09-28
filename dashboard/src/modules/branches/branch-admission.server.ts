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
import { isEmptyValue } from "#/modules/environment-design/saved-intent";
import { ancestors } from "#/modules/project/environment-tree";
import { liveOwner } from "./live-owner";

type Producers = SavedDeploymentTarget["variableProducers"];

/**
 * A Branch's attempt as admission writes it, captured fresh on every attempt: plus each never-deployed Own Copy's Setup
 * Commands by service id, the values of the Live Nodes its Own Copies reference, and the ones no ancestor provides or
 * that resolved empty. Any Environment's variables with an empty value count as missing too.
 */
export const branchAdmission = Effect.fn("Branches.branchAdmission")(function* (
  environmentId: string,
  target: SavedDeploymentTarget,
) {
  const live = yield* liveValuesOf(environmentId, target);
  const missingLiveValues = [...live.missingLiveValues, ...yield* emptyValuesOf(target)];
  return { ...target, ...live, missingLiveValues, setupCommands: yield* setupCommandsOf(environmentId) } satisfies SavedDeploymentTarget;
});

/** Its services' variables whose value is empty, such as one a pull request landed without a value: shown, never blocking. */
const emptyValuesOf = Effect.fn("Branches.emptyValuesOf")(function* (target: SavedDeploymentTarget) {
  const empty = target.variableProducers.filter(({ value }) => isEmptyValue(value));
  if (empty.length === 0) return [];
  const { drizzle } = yield* Database;
  const names = new Map((yield* drizzle.select({ id: service.id, name: service.name }).from(service)
    .where(inArray(service.id, empty.map((producer) => producer.ownerId)))).map((row) => [row.id, row.name]));
  return empty.map((producer): MissingLiveValue => ({ serviceId: producer.ownerId, from: names.get(producer.ownerId) ?? "", key: producer.key }));
});

/**
 * A Branch's ancestors in one read: each one's namespace, its Applied nodes by lineage (per node, so a node a partly failed
 * attempt deployed counts, each with the attempt that applied it), and the projection they came from. Nothing for a root.
 */
export const loadAncestorApplied = Effect.fn("Branches.loadAncestorApplied")(function* (environmentId: string) {
  const { drizzle } = yield* Database;
  const [self] = yield* drizzle.select({
    organizationId: environmentBranch.organizationId, projectId: environmentBranch.projectId, parentId: environmentBranch.parentEnvironmentId,
  }).from(environmentBranch).where(eq(environmentBranch.environmentId, environmentId));
  const runs = new Map<string, Map<string, AppliedSavedNode>>();
  if (!self) return { organizationId: null, parentId: null, branches: [], namespaces: new Map<string, string>(), runs, projection: null };
  const branches = yield* drizzle.select({ environmentId: environmentBranch.environmentId, parentEnvironmentId: environmentBranch.parentEnvironmentId })
    .from(environmentBranch).where(eq(environmentBranch.projectId, self.projectId));
  const ids = ancestors(self.parentId, branches);
  const namespaces = new Map((yield* drizzle.select({ id: environment.id, namespace: environment.namespace })
    .from(environment).where(inArray(environment.id, ids))).map((row) => [row.id, row.namespace]));
  const projection = yield* loadEnvironmentSnapshotProjection({ kind: "environments", environmentIds: ids });
  for (const node of projection.appliedSavedNodeByKey.values()) {
    runs.set(node.environmentId, (runs.get(node.environmentId) ?? new Map()).set(node.nodeLineageId, node));
  }
  return { organizationId: self.organizationId, parentId: self.parentId, branches, namespaces, runs, projection };
});

type Attempt = { id: string; createdAt: Date; producers: Producers | null };

/**
 * What an owner provides to what uses its nodes live: each node it runs, with its values from the attempt that applied it
 * there (a Live Node's values may read the owner's other nodes); what the owner itself uses live, as the newest of those
 * attempts captured it; and each of its services' public addresses.
 */
function ownerProducers(nodes: ReadonlyMap<string, AppliedSavedNode>, attempts: readonly Attempt[], clusterDomain: string | null): Producers {
  const attemptOf = new Map(attempts.map((attempt) => [attempt.id, attempt]));
  const newest = [...nodes.values()].map((node) => attemptOf.get(node.environmentDeploymentId))
    .reduce<Attempt | undefined>((latest, attempt) => (!attempt || (latest && latest.createdAt > attempt.createdAt) ? latest : attempt), undefined);
  return [
    ...[...nodes].flatMap(([lineage, node]) => (attemptOf.get(node.environmentDeploymentId)?.producers ?? [])
      .filter((producer) => producer.ownerLineageId === lineage)),
    ...(newest?.producers ?? []).filter((producer) => !nodes.has(producer.ownerLineageId)),
    ...publicDomainProducers(nodes, clusterDomain),
  ];
}

/**
 * Each service's PLOYZ_PUBLIC_DOMAIN, which a deploy resolves from the service's own configuration rather than recording
 * it, for every service the owner runs: a value that embeds it (an ORIGIN) reaches it through another service.
 */
function publicDomainProducers(nodes: ReadonlyMap<string, AppliedSavedNode>, clusterDomain: string | null): Producers {
  return [...nodes].flatMap(([lineage, node]) => {
    const domain = node.nodeType === "service" ? servicePublicDomain(parseServiceConfig(node.config), clusterDomain) : null;
    return domain ? [{
      ownerScope: "service" as const, ownerId: node.nodeId, ownerLineageId: lineage, key: "PLOYZ_PUBLIC_DOMAIN",
      value: { kind: "literal" as const, value: domain },
    }] : [];
  });
}

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
  const { organizationId, parentId, branches, namespaces, runs } = yield* loadAncestorApplied(environmentId);
  const byOwner = new Map<string, string[]>();
  const missing: { lineageId: string; key: string }[] = [];
  for (const lineageId of uses.keys()) {
    const owner = parentId ? liveOwner(parentId, lineageId, branches, runs) : null;
    if (owner) byOwner.set(owner, [...byOwner.get(owner) ?? [], lineageId]);
    else for (const key of uses.get(lineageId)?.keys() ?? []) missing.push({ lineageId, key });
  }
  const attemptIds = [...new Set([...byOwner.keys()].flatMap((owner) => [...runs.get(owner)?.values() ?? []].map((node) => node.environmentDeploymentId)))];
  const attempts = attemptIds.length ? yield* drizzle.select({
    id: environmentDeployment.id, createdAt: environmentDeployment.createdAt, producers: environmentDeployment.variableProducers,
  }).from(environmentDeployment).where(inArray(environmentDeployment.id, attemptIds)) : [];
  const clusterDomain = organizationId && byOwner.size ? (yield* loadClusterDomain(organizationId))?.name ?? null : null;
  for (const [owner, lineages] of byOwner) {
    const result = liveValues({
      owner: { namespace: namespaces.get(owner) ?? "", producers: ownerProducers(runs.get(owner) ?? new Map(), attempts, clusterDomain) },
      lineages: lineages.map((lineageId) => ({ lineageId, keys: [...uses.get(lineageId)?.keys() ?? []] })),
    });
    variableProducers.push(...result.producers);
    missing.push(...result.missing);
    // A Live value that resolved empty, such as one a pull request landed without a value, is missing to its readers too.
    missing.push(...result.producers.filter((producer) => isEmptyValue(producer.value) && uses.get(producer.ownerLineageId)?.has(producer.key))
      .map((producer) => ({ lineageId: producer.ownerLineageId, key: producer.key })));
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
