import type { DockerVolume } from "@ployz/sdk";

export const RUNTIME_VOLUME_REQUEST_TIMEOUT_MS = 5_000;

export type RuntimeVolumeSnapshot = {
  machine_id: string;
  name: string;
  labels: Record<string, string>;
};

export function runtimeVolumeSnapshotFromWatch(
  volume: DockerVolume,
): RuntimeVolumeSnapshot {
  return {
    machine_id: volume.id.machine_id,
    name: volume.id.name,
    labels: { ...volume.labels },
  };
}
