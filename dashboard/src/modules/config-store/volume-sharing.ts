import type { JsonValue, SettingRow, VolumeListing } from "@ployz/sdk";
import { desiredReplicas, replicaRows } from "./replica-rows";

// The Store refuses a second writer unless the Volume allows shared writes; these say so before the user asks, and warn
// where a Volume allows it, or where writers predate the rule.

/** The Volume that holds a Service to one replica: one it mounts that doesn't allow shared writes. */
export function replicaCap<V extends Pick<VolumeListing, "name" | "mounts" | "shared_writes">>(service: string, volumes: readonly V[]): V | null {
  return volumes.find((volume) => !volume.shared_writes && volume.mounts.some((mount) => mount.service === service)) ?? null;
}

/**
 * Why a Service can't mount a Volume, when it would be a second writer the Volume doesn't allow: another Service
 * already mounts it, or this one runs more than one replica. Null when it can.
 */
export function mountRefusal(volume: Pick<VolumeListing, "mounts" | "shared_writes">, service: string, replicas: number) {
  if (volume.shared_writes) return null;
  const other = volume.mounts.find((mount) => mount.service !== service);
  if (other) return `Already used by ${other.service}`;
  return replicas > 1 ? `Runs ${replicas} replicas` : null;
}

/** A Service's replica count as the next Deploy runs it, across regions: a staged value counts; unknown reads as 1. */
export function replicaCount(row: Pick<SettingRow, "value" | "default"> | undefined) {
  return desiredReplicas(row?.value ?? row?.default ?? 1) ?? 1;
}

export type VolumeWriter = { service: string; replicas: number };

/**
 * The containers that write a Volume: each mounting Service, as many times as it has replicas. More than one writer
 * shares one directory on one Server.
 */
export function volumeWriters(volume: Pick<VolumeListing, "mounts">, replicasOf: (service: string) => number) {
  const writers: VolumeWriter[] = [...new Set(volume.mounts.map((mount) => mount.service))]
    .map((service) => ({ service, replicas: replicasOf(service) }));
  const total = writers.reduce((sum, writer) => sum + writer.replicas, 0);
  return { writers, total, shared: total > 1 };
}

/** The writers in one line: "postgres ×2, redis". */
export const writersText = (writers: readonly VolumeWriter[]) =>
  writers.map(({ service, replicas }) => replicas > 1 ? `${service} ×${replicas}` : service).join(", ");

/** The region a Service has to run in because Services it shares a Volume with are kept there, and who they are. */
export type SharedRegion = { readonly region: string; readonly sharers: readonly string[]; readonly volume: string };

/**
 * Where `service` has to run: the one region another Service mounting the same Volume is kept in, as the next Deploy
 * has it. A Volume is on one Server, so the Deploy refuses Services that share it in different regions. Null when no
 * sharer is kept in one region.
 */
export function sharedRegion(service: string, volumes: readonly Pick<VolumeListing, "name" | "mounts">[], replicasOf: (service: string) => JsonValue, home: string | null): SharedRegion | null {
  for (const volume of volumes) {
    if (!volume.mounts.some((mount) => mount.service === service)) continue;
    const others = [...new Set(volume.mounts.map((mount) => mount.service))].filter((other) => other !== service);
    const kept = others.flatMap((other) => {
      const [only, ...more] = replicaRows(replicasOf(other), home);
      return only?.region != null && more.length === 0 ? [{ other, region: only.region }] : [];
    });
    const [first] = kept;
    if (first === undefined) continue;
    return { region: first.region, sharers: kept.filter(({ region }) => region === first.region).map(({ other }) => other), volume: volume.name };
  }
  return null;
}
