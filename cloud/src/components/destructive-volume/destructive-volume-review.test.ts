import { describe, expect, it } from "vitest";
import type { DestructiveVolumeReview } from "#/modules/deployments/deployment-contract";
import { prepareVolumeDestructionReview } from "./destructive-volume-review";

const review: DestructiveVolumeReview = {
  target: {
    version: 1,
    resourceId: "volume-resource-a",
    namespaceId: "production",
    volumeName: "database",
    machineId: "machine-a",
  },
  evidence: {
    version: 1,
    fingerprint: "volume-a",
    reviewedAt: "2026-07-17T00:00:00.000Z",
    evidence: {
      namespaceId: "production",
      volumeName: "database",
      machineId: "machine-a",
      kind: {
        kind: "provisioned",
        dataset: "ployz/production/database",
        maxSizeBytes: 10_000,
      },
      availability: {
        status: "available",
        usedBytes: 512,
        lastWriteUnixSeconds: 1_700_000_000,
      },
      referencingServices: ["web", "api"],
    },
  },
};

describe("prepareVolumeDestructionReview", () => {
  it("projects the exact reviewed target set and service references", () => {
    const prepared = prepareVolumeDestructionReview({
      reviews: [review],
      expectedResourceIds: ["volume-resource-a"],
      expectedNamespaceId: "production",
    });

    expect(prepared.reviews).toEqual([review]);
    expect(prepared.referencingServices).toEqual(["api", "web"]);
    expect(prepared.volumes[0]?.evidence.availability).toEqual({
      status: "available",
      usedBytes: 512,
      lastWriteUnixSeconds: 1_700_000_000,
    });
  });

  it("rejects a gather for a stale set of tombstoned resources", () => {
    expect(() =>
      prepareVolumeDestructionReview({
        reviews: [review],
        expectedResourceIds: ["volume-resource-b"],
        expectedNamespaceId: "production",
      }),
    ).toThrow(/changed/);
  });

  it("rejects testimony that does not match the authorized target", () => {
    expect(() =>
      prepareVolumeDestructionReview({
        reviews: [
          {
            ...review,
            evidence: {
              ...review.evidence,
              evidence: {
                ...review.evidence.evidence,
                machineId: "machine-b",
              },
            },
          },
        ],
        expectedResourceIds: ["volume-resource-a"],
        expectedNamespaceId: "production",
      }),
    ).toThrow(/does not match/);
  });

});
