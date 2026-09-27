import "@tanstack/react-start/server-only";
import { and, desc, eq, inArray } from "drizzle-orm";
import { Effect } from "effect";
import { liveValues } from "@ployz/sdk/config";
import { Database } from "#/server/database.server";
import { environment, environmentBranch } from "#/modules/project/tables";
import { serviceLineage } from "#/modules/environment-design/tables";
import type { CompiledSavedEnvironmentIntent } from "#/modules/environment-design/saved-intent";
import { environmentDeployment, type MissingLiveValue } from "#/modules/deployments/tables";
import { environmentNodeConfigSnapshot } from "#/modules/runtime/tables";

type Producers = CompiledSavedEnvironmentIntent["variableProducers"];

/**
 * What admission adds to a Branch's attempt, captured fresh on every attempt: the values of the Live Nodes its Own Copies
 * reference, from the nearest ancestor that runs each, and the ones no ancestor provides. A root gets nothing added.
 */
export const branchAdmission = Effect.fn("Branches.branchAdmission")(function* (
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
  const pending = new Set(uses.keys());
  // The owner of a Live lineage is the nearest ancestor whose latest applied attempt runs it.
  let ancestorId = yield* parentOf(environmentId);
  while (ancestorId !== null && pending.size > 0) {
    const [ancestor] = yield* drizzle.select({ namespace: environment.namespace }).from(environment).where(eq(environment.id, ancestorId));
    const [applied] = yield* drizzle.select({ id: environmentDeployment.id, producers: environmentDeployment.variableProducers })
      .from(environmentDeployment)
      .where(and(eq(environmentDeployment.environmentId, ancestorId), eq(environmentDeployment.status, "applied")))
      .orderBy(desc(environmentDeployment.createdAt), desc(environmentDeployment.id))
      .limit(1);
    if (ancestor && applied) {
      const runs = new Set((yield* drizzle.select({ lineageId: environmentNodeConfigSnapshot.nodeLineageId })
        .from(environmentNodeConfigSnapshot)
        .where(and(
          eq(environmentNodeConfigSnapshot.environmentDeploymentId, applied.id),
          inArray(environmentNodeConfigSnapshot.nodeLineageId, [...pending]),
        ))).map((row) => row.lineageId));
      if (runs.size > 0) {
        const result = liveValues({
          owner: { namespace: ancestor.namespace, producers: applied.producers ?? [] },
          lineages: [...runs].map((lineageId) => ({ lineageId, keys: [...uses.get(lineageId)?.keys() ?? []] })),
        });
        variableProducers.push(...result.producers);
        missing.push(...result.missing);
        for (const lineageId of runs) pending.delete(lineageId);
      }
    }
    ancestorId = yield* parentOf(ancestorId);
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

function parentOf(environmentId: string) {
  return Effect.gen(function* () {
    const { drizzle } = yield* Database;
    const [row] = yield* drizzle.select({ parentId: environmentBranch.parentEnvironmentId }).from(environmentBranch)
      .where(eq(environmentBranch.environmentId, environmentId));
    return row?.parentId ?? null;
  });
}
