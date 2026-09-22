import type { DeployIntent } from "@ployz/sdk";
import type { EnvironmentSnapshotVariableProducer } from "#/modules/environment-design/tables";
import type { EnvironmentDeploySnapshot } from "./runtime-contract";

/** Infer ordering from frozen references, dropping only edges that belong to a cycle. */
export function deploymentDependencies(
  snapshots: readonly EnvironmentDeploySnapshot[],
  producers: readonly EnvironmentSnapshotVariableProducer[],
): DeployIntent["dependencies"] {
  const services = new Map(snapshots
    .filter((snapshot) => snapshot.config.source.type !== "empty")
    .map((snapshot) => [snapshot.serviceId, snapshot.config]));
  const owners = new Map(producers
    .filter((producer) => producer.ownerScope === "service")
    .map((producer) => [producer.ownerLineageId, producer.ownerId]));
  const graph = new Map<string, Set<string>>();
  for (const config of services.values()) {
    const dependencies = new Set<string>();
    for (const value of Object.values(config.env)) {
      if (value.kind !== "literal") continue;
      for (const part of value.parts ?? []) {
        if (part.kind !== "ref" || part.owner.scope !== "service") continue;
        const ownerId = owners.get(part.owner.lineageId);
        const dependency = ownerId === undefined ? undefined : services.get(ownerId);
        if (dependency && dependency.privateDns !== config.privateDns) dependencies.add(dependency.privateDns);
      }
    }
    graph.set(config.privateDns, dependencies);
  }

  // ponytail: per-edge reachability keeps this small; use SCCs if large environments make it costly.
  const reaches = (from: string, target: string): boolean => {
    const pending = [from];
    const visited = new Set<string>();
    while (pending.length > 0) {
      const name = pending.pop();
      if (name === undefined) break;
      if (name === target) return true;
      if (visited.has(name)) continue;
      visited.add(name);
      pending.push(...(graph.get(name) ?? []));
    }
    return false;
  };
  const configs = new Map([...services.values()].map((config) => [config.privateDns, config]));
  const result: DeployIntent["dependencies"] = {};
  for (const [name, dependencies] of graph) {
    const edges = [...dependencies].filter((dependency) => !reaches(dependency, name)).sort();
    if (edges.length === 0) continue;
    result[name] = edges.map((service) => ({
      service,
      // Normal startup already monitors Docker health. An explicit HTTP check also gates unchanged dependencies.
      condition: configs.get(service)?.healthcheck.type === "http" ? "service_healthy" : "service_started",
    }));
  }
  return result;
}
