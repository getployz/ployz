import { describe, expect, it } from "vitest";
import {
  fingerprintReviewedEnvironmentWorkingState,
  getDestructiveEnvironmentSaveReviewMismatch,
  projectDestructiveEnvironmentSave,
  type ReviewableNodeIdentity,
  type ReviewedEnvironmentWorkingState,
} from "#/modules/environment-design/working-state-review";

describe("reviewed Environment Working State", () => {
  it("reviews only deployed Service and Volume removals that are newly absent from Saved State", () => {
    const node = (
      nodeType: "service" | "volume",
      nodeId: string,
      config: ReviewableNodeIdentity["config"] = {},
    ): ReviewableNodeIdentity => ({
      nodeType,
      nodeId,
      config,
    });

    expect(
      projectDestructiveEnvironmentSave({
        workingNodes: [
          node("service", "working-service"),
          node("service", "removed-service", null),
          node("volume", "removed-volume", null),
        ],
        savedNodes: [
          node("service", "working-service"),
          node("service", "removed-service"),
          node("volume", "removed-volume"),
          node("volume", "saved-only-volume"),
        ],
        appliedNodes: [
          node("service", "removed-service"),
          node("volume", "removed-volume"),
        ],
      }),
    ).toEqual({
      serviceIds: ["removed-service"],
      volumeIds: ["removed-volume"],
    });
  });

  it("requires exact reviewed destructive node sets", () => {
    expect(
      getDestructiveEnvironmentSaveReviewMismatch({
        expected: { serviceIds: ["service-a"], volumeIds: ["volume-a"] },
        reviewed: { serviceIds: ["service-a"], volumeIds: ["volume-a"] },
      }),
    ).toBeNull();
    expect(
      getDestructiveEnvironmentSaveReviewMismatch({
        expected: { serviceIds: ["service-a"], volumeIds: ["volume-a"] },
        reviewed: { serviceIds: [], volumeIds: ["volume-a"] },
      }),
    ).toContain("changed after review");
  });

  it("fingerprints the user-visible revision and ignores encrypted payload representation", async () => {
    const state = {
      nodeSnapshots: [
        {
          nodeType: "service" as const,
          nodeId: "00000000-0000-4000-8000-000000000001",
          nodeLineageId: "00000000-0000-4000-8000-000000000002",
          configVersion: 1,
          config: {
            name: "API",
            env: {
              TOKEN: {
                kind: "secret",
                variableId: "00000000-0000-4000-8000-000000000003",
                fingerprint: "visible-fingerprint",
                encryptedValue: { ciphertext: "first" },
              },
            },
          },
        },
      ],
      revisionMarkers: ["service:1"],
      tombstonedVolumeIds: [],
    };
    const reviewedNode = state.nodeSnapshots[0];
    if (!reviewedNode) throw new Error("Expected reviewed Service node.");
    const serverOnlyNode = {
      ...reviewedNode,
      environmentId: "00000000-0000-4000-8000-000000000004",
      encryptedRegistryUsername: {
        version: 1 as const,
        iv: "server-only",
        tag: "server-only",
        ciphertext: "server-only",
      },
      encryptedRegistrySecret: null,
    };

    const first = await fingerprintReviewedEnvironmentWorkingState(state);
    const serverProjection = await fingerprintReviewedEnvironmentWorkingState({
      ...state,
      nodeSnapshots: [serverOnlyNode],
    });
    const reencrypted = await fingerprintReviewedEnvironmentWorkingState({
      ...state,
      nodeSnapshots: [
        {
          ...reviewedNode,
          config: {
            ...reviewedNode.config,
            env: {
              TOKEN: {
                kind: "secret",
                variableId: "00000000-0000-4000-8000-000000000003",
                fingerprint: "visible-fingerprint",
                encryptedValue: { ciphertext: "second" },
              },
            },
          },
        },
      ],
    });
    const changed = await fingerprintReviewedEnvironmentWorkingState({
      ...state,
      revisionMarkers: ["service:2"],
    });

    expect(first).toMatch(/^environment-working-state-v1:[0-9a-f]{64}$/);
    expect(serverProjection).toBe(first);
    expect(reencrypted).toBe(first);
    expect(changed).not.toBe(first);
  });

  it("uses locale-independent ordering for reviewed nodes and non-ASCII config keys", async () => {
    const nodes: ReviewedEnvironmentWorkingState["nodeSnapshots"] = [
      {
        nodeType: "service" as const,
        nodeId: "00000000-0000-4000-8000-000000000020",
        nodeLineageId: "00000000-0000-4000-8000-000000000021",
        configVersion: 1,
        config: { "ä-key": "first", "z-key": "second" },
      },
      {
        nodeType: "volume" as const,
        nodeId: "00000000-0000-4000-8000-000000000010",
        nodeLineageId: "00000000-0000-4000-8000-000000000011",
        configVersion: 2,
        config: { "é-key": "third", "a-key": "fourth" },
      },
    ];

    const forward = await fingerprintReviewedEnvironmentWorkingState({
      nodeSnapshots: nodes,
      revisionMarkers: ["é-marker", "a-marker"],
      tombstonedVolumeIds: [],
    });
    const reversed = await fingerprintReviewedEnvironmentWorkingState({
      nodeSnapshots: [...nodes].reverse(),
      revisionMarkers: ["a-marker", "é-marker"],
      tombstonedVolumeIds: [],
    });

    expect(reversed).toBe(forward);
  });
});
