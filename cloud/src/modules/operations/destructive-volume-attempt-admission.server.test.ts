import { Effect, Exit } from "effect";
import { describe, expect, it } from "vitest";
import type { DestructiveVolumeAttemptRecord } from "#/modules/operations/destructive-volume-attempt-persistence.server";
import { retryOutcome } from "#/modules/operations/destructive-volume-attempt-admission.server";

describe("destructive volume retry outcome", () => {
  it("fails with a typed conflict when Core identity is lost", async () => {
    const source = attemptRecord({
      operationId: "operation-1",
      startSequence: "4",
    });
    const attempt = attemptRecord({ operationId: null, startSequence: null });

    const exit = await Effect.runPromise(
      retryOutcome("created", "reobserve_operation", source, attempt).pipe(
        Effect.exit,
      ),
    );

    expect(Exit.isFailure(exit)).toBe(true);
    if (Exit.isFailure(exit)) {
      expect(exit.cause.toString()).toContain("DestructiveVolumeConflict");
    }
  });

  it("returns the associated retry identity without throwing", async () => {
    const source = attemptRecord({
      operationId: "operation-1",
      startSequence: "4",
    });
    const attempt = attemptRecord({
      operationId: "operation-1",
      startSequence: "4",
    });

    await expect(
      Effect.runPromise(
        retryOutcome("created", "reobserve_operation", source, attempt),
      ),
    ).resolves.toEqual({
      state: "created",
      mode: "reobserve_operation",
      attempt,
      operationId: "operation-1",
      startSequence: "4",
      txid: undefined,
    });
  });
});

function attemptRecord(
  identity: Pick<
    DestructiveVolumeAttemptRecord,
    "operationId" | "startSequence"
  >,
): DestructiveVolumeAttemptRecord {
  return {
    id: "attempt-1",
    environmentDeploymentId: "deployment-1",
    environmentResourceId: "resource-1",
    retryOfAttemptId: null,
    target: {
      version: 1,
      resourceId: "resource-1",
      namespaceId: "namespace-1",
      volumeName: "volume-1",
      machineId: "machine-1",
    },
    evidence: {
      version: 1,
      fingerprint: "fingerprint-1",
      reviewedAt: "2026-07-10T11:00:00.000Z",
      evidence: {
        namespaceId: "namespace-1",
        volumeName: "volume-1",
        machineId: "machine-1",
        kind: { kind: "plain" },
        availability: { status: "no_answer" },
        referencingServices: [],
      },
    },
    evidenceFingerprint: "fingerprint-1",
    disposition: "accepted",
    ...identity,
    inngestRunId: null,
    requestPublishedAt: null,
    acceptedAt: new Date("2026-07-10T11:00:00.000Z"),
    terminalEvent: null,
    failure: null,
    deadlineAt: null,
    terminalAt: null,
    createdAt: new Date("2026-07-10T11:00:00.000Z"),
    updatedAt: new Date("2026-07-10T11:00:00.000Z"),
  };
}
