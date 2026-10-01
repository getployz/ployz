import type { SettingRow, VolumeListing } from "@ployz/sdk";

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

/** A Service's replica count as the next Deploy runs it: a staged value counts; unknown reads as 1. */
export function replicaCount(row: Pick<SettingRow, "value" | "default"> | undefined) {
  const count = Number(row?.value ?? row?.default ?? 1);
  return Number.isInteger(count) && count >= 0 ? count : 1;
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
