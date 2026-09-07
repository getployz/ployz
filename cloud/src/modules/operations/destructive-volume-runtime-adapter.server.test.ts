import { describe, expect, it } from "vitest";
import { Effect } from "effect";
import { SdkSurfaceNotShipped } from "#/modules/runtime/ployz.server";
import {
  recoverDestructiveVolumeAcceptance,
  submitDestructiveVolume,
  verifyFreshDestructiveVolumeEvidence,
  watchDestructiveVolumeBatch,
} from "./destructive-volume-runtime-adapter.server";

const target = {
  version: 1 as const,
  resourceId: "resource-1",
  namespaceId: "namespace-1",
  volumeName: "vol-1",
  machineId: "machine-1",
};

async function expectRemoveVolumesNotShipped<A, E>(result: Effect.Effect<A, E>) {
  const value = await Effect.runPromise(result.pipe(Effect.flip));
  expect(value).toBeInstanceOf(SdkSurfaceNotShipped);
  expect(value).toMatchObject({
    surface: "removeVolumes",
    ticket: "getployz/ployz2#352",
  });
}

describe("destructive volume runtime adapter", () => {
  it("does not ship removeVolumes until #352", async () => {
    await expectRemoveVolumesNotShipped(
      submitDestructiveVolume({
        attemptId: "attempt-1",
        target,
      }),
    );
    await expectRemoveVolumesNotShipped(
      recoverDestructiveVolumeAcceptance({
        attemptId: "attempt-1",
        target,
      }),
    );
    await expectRemoveVolumesNotShipped(
      verifyFreshDestructiveVolumeEvidence({
        target,
        evidence: {
          version: 1,
          fingerprint: "fp-1",
          reviewedAt: "2026-07-17T00:00:00.000Z",
          evidence: {
            namespaceId: "namespace-1",
            volumeName: "vol-1",
            machineId: "machine-1",
            kind: { kind: "plain" },
            availability: { status: "no_answer" },
            referencingServices: [],
          },
        },
      }),
    );
    await expectRemoveVolumesNotShipped(
      watchDestructiveVolumeBatch({
        organizationId: "org-1",
        operationId: "op-1",
        maxPages: 1,
      }),
    );
  });
});
