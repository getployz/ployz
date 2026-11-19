import type { JsonValue } from "@ployz/sdk";
import { parseDockerVolumeName } from "#/modules/volume-run/volume-run";
import { replicaRows } from "./replica-rows";

/** A Service the next Deploy leaves waiting, and why, in the reason `ployz deploy` then gives: `pg-data is on machine-1`. */
export type DeployWait = { readonly service: string; readonly why: string };

const anyOf = (names: readonly string[]) => (names.length < 2 ? names.join("") : `${names.slice(0, -1).join(", ")} or ${names.at(-1)}`);

/**
 * The Services the next Deploy leaves waiting for where they are kept, as far as the Servers seen say. A Service kept
 * in a region no Server is in waits for one. A Service kept in one region waits when a Volume it mounts is written on
 * a Server outside it, since the Deploy runs it where its Volume is written and moves no Volume. A mirror is no copy it
 * can run on. A Service in any region is kept nowhere. Nothing
 * waits before the Servers are known.
 */
export function deployWaits({ services, home, volumes, copies, servers }: {
  /** Each Service with its `replicas` as the next Deploy has it. */
  services: readonly { readonly name: string; readonly replicas: JsonValue }[];
  /** The Environment's region as the next Deploy has it. */
  home: string | null;
  volumes: readonly { readonly id: string; readonly name: string; readonly mounts: readonly { readonly service: string }[] }[];
  /** The copies of Volumes the Servers hold, by Docker volume name. */
  copies: readonly { readonly machineId: string; readonly name: string; readonly role: string }[];
  servers: readonly { readonly id: string; readonly name: string; readonly region: string | null }[];
}): DeployWait[] {
  if (servers.length === 0) return [];
  const regionOf = new Map(servers.map(({ id, region }) => [id, region?.toLowerCase() ?? null]));
  const filled = new Set(regionOf.values());
  return services.flatMap(({ name, replicas }): DeployWait[] => {
    const regions = replicaRows(replicas, home).flatMap(({ region, count }) => (region !== null && count > 0 ? [region] : []));
    const empty = regions.filter((region) => !filled.has(region));
    if (empty.length > 0) return [{ service: name, why: `no Server in ${anyOf(empty)}` }];
    const [region, ...others] = regions;
    if (region === undefined || others.length > 0) return [];
    for (const volume of volumes.filter(({ mounts }) => mounts.some(({ service }) => service === name))) {
      const writers = copies.filter((copy) => copy.role === "writer" && parseDockerVolumeName(copy.name)?.volumeId === volume.id && regionOf.has(copy.machineId));
      if (writers.length === 0 || writers.some(({ machineId }) => regionOf.get(machineId) === region)) continue;
      const server = servers.find(({ id }) => id === writers[0]?.machineId);
      if (server !== undefined) return [{ service: name, why: `${volume.name} is on ${server.name}` }];
    }
    return [];
  });
}
