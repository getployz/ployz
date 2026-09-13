import { describe, expect, it } from "vitest";
import type { RuntimeVolumeSnapshot } from "#/modules/runtime/runtime-volume";
import {
  equalDestructiveVolumeEvidence,
  fingerprintDestructiveVolume,
  prepareDestructiveVolumeEvidence,
  prepareNamespaceDestructiveEvidence,
  presentDestructiveVolumeEvidence,
} from "#/modules/operations/destructive-volume-evidence";

const namespaceId = "namespace-1";

function snapshot(
  overrides: Partial<RuntimeVolumeSnapshot> = {},
): RuntimeVolumeSnapshot {
  return {
    machine_id: "machine-1",
    name: "database",
    labels: {},
    ...overrides,
  };
}

describe("destructive volume evidence", () => {
  it("presents watch identity as plain with no invented testimony", () => {
    expect(presentDestructiveVolumeEvidence(snapshot(), namespaceId)).toEqual({
      namespaceId: "namespace-1",
      volumeName: "database",
      machineId: "machine-1",
      kind: { kind: "plain" },
      availability: { status: "no_answer" },
      referencingServices: [],
    });
  });

  it("fingerprints machine and name without inventing size or recency", () => {
    const first = snapshot({ labels: { used: "512" } });
    const refreshed = snapshot({ labels: { used: "1024" } });

    expect(fingerprintDestructiveVolume(first)).toBe(
      '["machine-1","database"]',
    );
    expect(fingerprintDestructiveVolume(refreshed)).toBe(
      fingerprintDestructiveVolume(first),
    );
    expect(
      fingerprintDestructiveVolume(snapshot({ machine_id: "machine-2" })),
    ).not.toBe(fingerprintDestructiveVolume(first));
  });

  it("prepares display evidence and its submission identity together", () => {
    const volume = snapshot();

    expect(prepareDestructiveVolumeEvidence(volume, namespaceId)).toEqual({
      evidence: presentDestructiveVolumeEvidence(volume, namespaceId),
      fingerprint: fingerprintDestructiveVolume(volume),
    });
  });

  it("compares identity fields that Cloud still owns", () => {
    const evidence = presentDestructiveVolumeEvidence(snapshot(), namespaceId);

    expect(equalDestructiveVolumeEvidence(evidence, { ...evidence })).toBe(true);
    expect(
      equalDestructiveVolumeEvidence(evidence, {
        ...evidence,
        machineId: "machine-2",
      }),
    ).toBe(false);
    expect(
      equalDestructiveVolumeEvidence(evidence, {
        ...evidence,
        volumeName: "assets",
      }),
    ).toBe(false);
  });

  it("prepares namespace volumes in stable machine-then-name order", () => {
    const database = snapshot();
    const assets = snapshot({ name: "assets" });

    const prepared = prepareNamespaceDestructiveEvidence(namespaceId, [
      database,
      assets,
    ]);
    const reversed = prepareNamespaceDestructiveEvidence(namespaceId, [
      assets,
      database,
    ]);

    expect(prepared.volumes.map(({ evidence }) => evidence.volumeName)).toEqual([
      "assets",
      "database",
    ]);
    expect(prepared.referencingServices).toEqual([]);
    expect(reversed.fingerprint).toBe(prepared.fingerprint);
    expect(
      prepareNamespaceDestructiveEvidence(namespaceId, []),
    ).toMatchObject({
      namespaceId: "namespace-1",
      volumes: [],
      referencingServices: [],
    });
  });
});
