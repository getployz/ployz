import type { JsonValue } from "@ployz/sdk";
import { parseDockerVolumeName } from "#/modules/volume-run/volume-run";
import { replicaRows } from "./replica-rows";

/** A Service the next Deploy leaves waiting, or refuses, and why, in the reason `ployz deploy` then gives. */
export type DeployWait = { readonly service: string; readonly why: string; readonly refused: boolean };

const listOf = (word: string) => (names: readonly string[]) => (names.length < 2 ? names.join("") : `${names.slice(0, -1).join(", ")} ${word} ${names.at(-1)}`);
const anyOf = listOf("or");
const allOf = listOf("and");

type Lens = {
  /** Each Service with its `replicas` as the next Deploy has it. */
  services: readonly { readonly name: string; readonly replicas: JsonValue }[];
  /** The Environment's region as the next Deploy has it. */
  home: string | null;
  volumes: readonly SharedVolume[];
  /** The copies of Volumes the Servers hold, by Docker volume name. */
  copies: readonly { readonly machineId: string; readonly name: string; readonly role: string }[];
  servers: readonly { readonly id: string; readonly name: string; readonly region: string | null }[];
};

type SharedVolume = { readonly id: string; readonly name: string; readonly mounts: readonly { readonly service: string }[] };

/** Where Services that share a Volume have to run: the region of the Server that writes it, or that Server when it has none. */
type Home = { readonly region: string } | { readonly server: string };

function read({ services, home, copies, servers }: Lens) {
  const regionOf = new Map(servers.map(({ id, region }) => [id, region?.toLowerCase() ?? null]));
  const keptIn = new Map(services.map(({ name, replicas }) => {
    const rows = replicaRows(replicas, home);
    return [name, rows.some(({ region }) => region === null) ? null : rows.flatMap(({ region, count }) => (region !== null && count > 0 ? [region] : []))];
  }));
  const writersOf = (volume: string) => copies
    .filter((copy) => copy.role === "writer" && parseDockerVolumeName(copy.name)?.volumeId === volume && regionOf.has(copy.machineId))
    .sort((a, b) => a.machineId.localeCompare(b.machineId));
  const homeOf = (volume: string): Home | null => {
    const server = servers.find(({ id }) => id === writersOf(volume)[0]?.machineId);
    if (server === undefined) return null;
    const region = regionOf.get(server.id);
    return region ? { region } : { server: server.name };
  };
  return { regionOf, keptIn, writersOf, homeOf };
}

/**
 * Why the Deploy refuses `name`, kept in `region`, when `volume` is not written there and another Service sharing it is
 * kept away from that region, since no one Server could ever suit both, in a few words and in full. Null when it
 * doesn't. It says so as the Engine's planner does (`shared_elsewhere` in core/crates/ployz/src/deploy/planning/volumes.rs).
 */
function splitRefusal(name: string, region: string, volume: SharedVolume, lens: ReturnType<typeof read>) {
  const { regionOf, keptIn, writersOf, homeOf } = lens;
  const sharers = [...new Set(volume.mounts.map(({ service }) => service))].sort();
  if (!sharers.some((sharer) => keptIn.get(sharer)?.includes(region) === false)) return null;
  if (writersOf(volume.id).some(({ machineId }) => regionOf.get(machineId) === region)) return null;
  const kept = sharers.flatMap((sharer) => {
    const regions = keptIn.get(sharer);
    return sharer !== name && regions?.length ? [{ sharer, regions }] : [];
  });
  const home = homeOf(volume.id);
  const short = home === null ? `Must run with ${allOf(kept.map(({ sharer }) => sharer))}`
    : "region" in home ? `Must run in ${home.region}` : `${home.server} has no region`;
  const why = home === null
    ? `${volume.name} isn't made yet. ${allOf([...kept.map(({ sharer, regions }) => `${sharer} is set to ${allOf(regions)}`), `${name} is set to ${region}`])}. Set them all to one region`
    : "region" in home ? `${volume.name} is in ${home.region} and ${name} is set to ${region}. Set ${name} to ${home.region}`
    : `${volume.name} is on ${home.server}, which has no region. Give ${home.server} a region, then set them all to it`;
  return { short, why: `${allOf([name, ...kept.map(({ sharer }) => sharer)])} share the Volume ${volume.name}, so they have to run in the same region. ${why}` };
}

/**
 * The Services the next Deploy leaves waiting for where they are kept, as far as the Servers seen say. A Service kept
 * in a region no Server is in waits for one. A Service kept in one region waits when a Volume it mounts is written on
 * a Server outside it, since the Deploy runs it where its Volume is written and moves no Volume. A mirror is no copy it
 * can run on. A Service in any region is kept nowhere. Nothing waits before the Servers are known.
 *
 * The Deploy refuses, before any of that, a Service that would run apart from a Service it shares a Volume with.
 */
export function deployWaits(lens: Lens): DeployWait[] {
  const { services, volumes, servers } = lens;
  if (servers.length === 0) return [];
  const seen = read(lens);
  const { regionOf, keptIn, writersOf } = seen;
  const filled = new Set(regionOf.values());
  return services.flatMap(({ name }): DeployWait[] => {
    const regions = keptIn.get(name) ?? [];
    const [region, ...others] = regions;
    const mounted = volumes.filter(({ mounts }) => mounts.some(({ service }) => service === name));
    if (region !== undefined && others.length === 0) {
      for (const volume of mounted) {
        const refusal = splitRefusal(name, region, volume, seen);
        if (refusal !== null) return [{ service: name, why: refusal.why, refused: true }];
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

/** Why the Deploy would refuse `service` once it mounts `volume`, in a few words and in the Deploy's. Null when it wouldn't. */
export function mountSplit(service: string, volume: SharedVolume, lens: Omit<Lens, "volumes">) {
  if (lens.servers.length === 0) return null;
  const seen = read({ ...lens, volumes: [] });
  const [region, ...others] = seen.keptIn.get(service) ?? [];
  if (region === undefined || others.length > 0) return null;
  return splitRefusal(service, region, { ...volume, mounts: [...volume.mounts, { service }] }, seen);
}

/** The region a Service has to run in because it shares a Volume, which Volume, who with, and whether the Volume is in it. */
export type SharedRegion = { readonly region: string; readonly volume: string; readonly sharers: readonly string[]; readonly located: boolean };

/**
 * Where `service` has to run because it shares a Volume: the region of the Server that writes the Volume. A Volume with
 * no region, not made yet or on a Server without one, holds its Services to the region they are all set to, when they
 * agree. Null when nothing decides.
 */
export function sharedRegion(service: string, lens: Lens): SharedRegion | null {
  const seen = read(lens);
  for (const volume of lens.volumes) {
    const all = [...new Set(volume.mounts.map((mount) => mount.service))].sort();
    const sharers = all.filter((other) => other !== service);
    if (sharers.length === all.length || sharers.length === 0) continue;
    const home = seen.homeOf(volume.id);
    if (home !== null && "region" in home) return { region: home.region, volume: volume.name, sharers, located: true };
    const regions = new Set(all.flatMap((sharer) => seen.keptIn.get(sharer) ?? []));
    const [only] = regions;
    if (only !== undefined && regions.size === 1) return { region: only, volume: volume.name, sharers, located: false };
  }
  return null;
}
