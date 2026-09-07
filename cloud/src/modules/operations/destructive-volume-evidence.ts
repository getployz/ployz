import type { RuntimeVolumeSnapshot } from "#/modules/runtime/runtime-volume";

export type DestructiveVolumeKind =
  | { kind: "plain" }
  | {
      kind: "provisioned";
      dataset: string;
      maxSizeBytes: number;
    };

export type DestructiveVolumeAvailability =
  | {
      status: "available";
      usedBytes: number;
      lastWriteUnixSeconds: number;
    }
  | { status: "unavailable" }
  | { status: "no_answer" };

export type DestructiveVolumeEvidence = {
  namespaceId: string;
  volumeName: string;
  machineId: string;
  kind: DestructiveVolumeKind;
  availability: DestructiveVolumeAvailability;
  referencingServices: string[];
};

export type PreparedDestructiveVolumeEvidence = {
  evidence: DestructiveVolumeEvidence;
  fingerprint: string;
};

export type PreparedNamespaceDestructiveEvidence = {
  namespaceId: string;
  volumes: PreparedDestructiveVolumeEvidence[];
  referencingServices: string[];
  fingerprint: string;
};

export function presentDestructiveVolumeEvidence(
  snapshot: RuntimeVolumeSnapshot,
  namespaceId: string,
): DestructiveVolumeEvidence {
  return {
    namespaceId,
    volumeName: snapshot.name,
    machineId: snapshot.machine_id,
    kind: { kind: "plain" },
    availability: { status: "no_answer" },
    referencingServices: [],
  };
}

export function fingerprintDestructiveVolume(
  snapshot: RuntimeVolumeSnapshot,
): string {
  return JSON.stringify([snapshot.machine_id, snapshot.name]);
}

export function prepareDestructiveVolumeEvidence(
  snapshot: RuntimeVolumeSnapshot,
  namespaceId: string,
): PreparedDestructiveVolumeEvidence {
  return {
    evidence: presentDestructiveVolumeEvidence(snapshot, namespaceId),
    fingerprint: fingerprintDestructiveVolume(snapshot),
  };
}

export function equalDestructiveVolumeEvidence(
  left: DestructiveVolumeEvidence,
  right: DestructiveVolumeEvidence,
): boolean {
  return (
    left.namespaceId === right.namespaceId &&
    left.volumeName === right.volumeName &&
    left.machineId === right.machineId &&
    left.kind.kind === right.kind.kind &&
    left.availability.status === right.availability.status
  );
}

export function prepareNamespaceDestructiveEvidence(
  namespaceId: string,
  snapshots: readonly RuntimeVolumeSnapshot[],
): PreparedNamespaceDestructiveEvidence {
  const volumes = [...snapshots]
    .sort(
      (left, right) =>
        compareText(left.machine_id, right.machine_id) ||
        compareText(left.name, right.name),
    )
    .map((snapshot) => prepareDestructiveVolumeEvidence(snapshot, namespaceId));
  const referencingServices = [
    ...new Set(volumes.flatMap(({ evidence }) => evidence.referencingServices)),
  ].sort();

  return {
    namespaceId,
    volumes,
    referencingServices,
    fingerprint: JSON.stringify([
      namespaceId,
      volumes.map(({ fingerprint }) => fingerprint),
    ]),
  };
}

function compareText(left: string, right: string): number {
  if (left < right) return -1;
  if (left > right) return 1;
  return 0;
}
