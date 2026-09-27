import "@tanstack/react-start/server-only";
import { and, desc, eq, inArray, isNull } from "drizzle-orm";
import { Effect } from "effect";
import { liveValues } from "@ployz/sdk/config";
import { Database } from "#/server/database.server";
import { environment, environmentBranch } from "#/modules/project/tables";
import { service, serviceLineage } from "#/modules/environment-design/tables";
import type { CompiledSavedEnvironmentIntent } from "#/modules/environment-design/saved-intent";
import { environmentDeployment, type MissingLiveValue } from "#/modules/deployments/tables";
import { environmentNodeConfigSnapshot } from "#/modules/runtime/tables";
import { liveOwner } from "./live-owner";

type Producers = CompiledSavedEnvironmentIntent["variableProducers"];

/**
 * What admission adds to a Branch's attempt, captured fresh on every attempt: each never-deployed Own Copy's Setup Commands
 * by service id, the values of the Live Nodes its Own Copies reference, and the ones no ancestor provides. A root gets nothing added.
 */
export const branchAdmission = Effect.fn("Branches.branchAdmission")(function* (
  environmentId: string,
  target: CompiledSavedEnvironmentIntent,
) {
  return { ...(yield* liveValuesOf(environmentId, target)), setupCommands: yield* setupCommandsOf(environmentId) };
});

/** From the nearest ancestor that runs each Live Node its Own Copies reference, and the ones no ancestor provides. */
const liveValuesOf = Effect.fn("Branches.liveValuesOf")(function* (
  environmentId: string,
  target: CompiledSavedEnvironmentIntent,
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

  const missing: { lineageId: string; key: string }[] = [];
  const [self] = yield* drizzle.select({ projectId: environmentBranch.projectId, parentId: environmentBranch.parentEnvironmentId })
    .from(environmentBranch).where(eq(environmentBranch.environmentId, environmentId));
  const branches = self
    ? yield* drizzle.select({ environmentId: environmentBranch.environmentId, parentEnvironmentId: environmentBranch.parentEnvironmentId })
      .from(environmentBranch).where(eq(environmentBranch.projectId, self.projectId))
    : [];
  // Each ancestor's latest applied attempt and which of the Live lineages it runs.
  const parentOf = new Map(branches.map((row) => [row.environmentId, row.parentEnvironmentId]));
  const attempts = new Map<string, { namespace: string; producers: Producers }>();
  const applied = new Map<string, Set<string>>();
  for (let at = self?.parentId; at && !attempts.has(at); at = parentOf.get(at)) {
    const [ancestor] = yield* drizzle.select({ namespace: environment.namespace }).from(environment).where(eq(environment.id, at));
    const [attempt] = yield* drizzle.select({ id: environmentDeployment.id, producers: environmentDeployment.variableProducers })
      .from(environmentDeployment)
      .where(and(eq(environmentDeployment.environmentId, at), eq(environmentDeployment.status, "applied")))
      .orderBy(desc(environmentDeployment.createdAt), desc(environmentDeployment.id))
      .limit(1);
    attempts.set(at, { namespace: ancestor?.namespace ?? "", producers: attempt?.producers ?? [] });
    if (!ancestor || !attempt) continue;
    applied.set(at, new Set((yield* drizzle.select({ lineageId: environmentNodeConfigSnapshot.nodeLineageId })
      .from(environmentNodeConfigSnapshot)
      .where(and(
        eq(environmentNodeConfigSnapshot.environmentDeploymentId, attempt.id),
        inArray(environmentNodeConfigSnapshot.nodeLineageId, [...uses.keys()]),
      ))).map((row) => row.lineageId)));
  }
  // The owner of a Live lineage is the nearest ancestor whose latest applied attempt runs it.
  const byOwner = new Map<string, string[]>();
  const pending: string[] = [];
  for (const lineageId of uses.keys()) {
    const owner = self ? liveOwner(self.parentId, lineageId, branches, applied) : null;
    if (owner) byOwner.set(owner, [...byOwner.get(owner) ?? [], lineageId]);
    else pending.push(lineageId);
  }
  for (const [owner, lineages] of byOwner) {
    const result = liveValues({
      owner: attempts.get(owner) ?? { namespace: "", producers: [] },
      lineages: lineages.map((lineageId) => ({ lineageId, keys: [...uses.get(lineageId)?.keys() ?? []] })),
    });
    variableProducers.push(...result.producers);
    missing.push(...result.missing);
  }
  for (const lineageId of pending) for (const key of uses.get(lineageId)?.keys() ?? []) missing.push({ lineageId, key });
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
