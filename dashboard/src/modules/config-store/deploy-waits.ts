import type { JsonValue } from "@ployz/sdk";
import { parseDockerVolumeName } from "#/modules/volume-run/volume-run";
import { replicaRows } from "./replica-rows";

/** A Service the next Deploy leaves waiting, or refuses, and why, in the reason `ployz deploy` then gives. */
export type DeployWait = { readonly service: string; readonly why: string; readonly refused: boolean };

const listOf = (word: string) => (names: readonly string[]) => (names.length < 2 ? names.join("") : `${names.slice(0, -1).join(", ")} ${word} ${names.at(-1)}`);
const anyOf = listOf("or");
const allOf = listOf("and");

/**
 * The Services the next Deploy leaves waiting for where they are kept, as far as the Servers seen say. A Service kept
 * in a region no Server is in waits for one. A Service kept in one region waits when a Volume it mounts is written on
 * a Server outside it, since the Deploy runs it where its Volume is written and moves no Volume. A mirror is no copy it
 * can run on. A Service in any region is kept nowhere. Nothing waits before the Servers are known.
 *
 * The Deploy refuses, before any of that, a Service kept in one region whose Volume is not written there while another
 * Service sharing it is kept away from that region, since no one Server could ever suit both. It says so as the
 * Engine's planner does (`shared_elsewhere` in core/crates/ployz/src/deploy/planning/volumes.rs).
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
  const keptIn = new Map(services.map(({ name, replicas }) => {
    const rows = replicaRows(replicas, home);
    return [name, rows.some(({ region }) => region === null) ? null : rows.flatMap(({ region, count }) => (region !== null && count > 0 ? [region] : []))];
  }));
  const writersOf = (volume: string) => copies
    .filter((copy) => copy.role === "writer" && parseDockerVolumeName(copy.name)?.volumeId === volume && regionOf.has(copy.machineId))
    .sort((a, b) => a.machineId.localeCompare(b.machineId));
  return services.flatMap(({ name }): DeployWait[] => {
    const regions = keptIn.get(name) ?? [];
    const [region, ...others] = regions;
    const mounted = volumes.filter(({ mounts }) => mounts.some(({ service }) => service === name));
    if (region !== undefined && others.length === 0) {
      for (const volume of mounted) {
        const sharers = [...new Set(volume.mounts.map(({ service }) => service))].sort();
        if (!sharers.some((sharer) => keptIn.get(sharer)?.includes(region) === false)) continue;
        const writers = writersOf(volume.id);
        if (writers.some(({ machineId }) => regionOf.get(machineId) === region)) continue;
        const at = writers[0] === undefined ? null : regionOf.get(writers[0].machineId);
        const why = at ? `${volume.name} is in ${at}, and ${name} runs in ${region}. Run ${name} in ${at} instead`
          : `${volume.name} is shared by ${allOf(sharers.flatMap((sharer) => {
            const kept = keptIn.get(sharer);
            return kept ? [`${sharer} in ${kept.length === 1 ? kept[0] : "no one region"}`] : [];
          }))}. Run them in one region, or give each its own Volume`;
        return [{ service: name, why, refused: true }];
      }
    }
    const empty = regions.filter((kept) => !filled.has(kept));
    if (empty.length > 0) return [{ service: name, why: `no Server in ${anyOf(empty)}`, refused: false }];
    if (region === undefined || others.length > 0) return [];
    for (const volume of mounted) {
      const writers = writersOf(volume.id);
      if (writers.length === 0 || writers.some(({ machineId }) => regionOf.get(machineId) === region)) continue;
      const server = servers.find(({ id }) => id === writers[0]?.machineId);
      if (server === undefined) continue;
      const at = regionOf.get(server.id);
      const why = at ? `its Volume ${volume.name} is in ${at} and can't move to ${region} yet. Run it in ${at} instead`
        : `its Volume ${volume.name} is on ${server.name}, a Server in no region, and can't move to ${region} yet. Give ${server.name} the region ${region} instead`;
      return [{ service: name, why, refused: false }];
    }
    return [];
  });
}
