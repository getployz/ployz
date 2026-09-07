import { describe, expect, it } from "vitest";
import { Effect, Exit } from "effect";
import {
  applyDestructiveVolumeAttemptEvent,
  destructiveVolumeRetryModeForAttempt,
  isDestructiveVolumeTerminalEvidenceEvent,
  normalizeDestructiveVolumeAttemptEvent,
  projectDestructiveVolumeWorkflowAttempt,
  projectDestructiveVolumeTerminalAttemptEvent,
  sameReviewedDestructiveVolumeIdentity,
  type DestructiveVolumeAttemptState,
} from "./destructive-volume-attempt";

const active: DestructiveVolumeAttemptState = { disposition: "active" };

describe("destructive volume attempt", () => {
  it("associates one accepted Core operation with its start sequence", () => {
    expect(
      applyDestructiveVolumeAttemptEvent(active, {
        event: "core_accepted",
        operationId: "operation-1",
        startSequence: "4",
      }),
    ).toEqual({
      disposition: "accepted",
      operationId: "operation-1",
      startSequence: "4",
    });
  });

  it("only completes reconciliation from this attempt's completed operation", () => {
    const accepted: DestructiveVolumeAttemptState = {
      disposition: "accepted",
      operationId: "operation-1",
      startSequence: "4",
    };

    expect(() =>
      applyDestructiveVolumeAttemptEvent(accepted, {
        event: "core_completed",
        operationId: "operation-other",
      }),
    ).toThrow("different Core operation");
    expect(
      applyDestructiveVolumeAttemptEvent(accepted, {
        event: "core_completed",
        operationId: "operation-1",
      }),
    ).toEqual({
      disposition: "completed",
      operationId: "operation-1",
      startSequence: "4",
    });
  });

  it("retains accepted Core evidence when Cloud reconciliation is partial", () => {
    const accepted: DestructiveVolumeAttemptState = {
      disposition: "accepted",
      operationId: "operation-1",
      startSequence: "4",
    };

    expect(
      applyDestructiveVolumeAttemptEvent(accepted, {
        event: "reconciliation_failed",
        operationId: "operation-1",
        code: "resource_identity_conflict",
        message: "The reviewed Cloud volume no longer matches.",
      }),
    ).toEqual({
      disposition: "partial",
      operationId: "operation-1",
      startSequence: "4",
      failure: {
        code: "resource_identity_conflict",
        message: "The reviewed Cloud volume no longer matches.",
      },
    });
  });

  it("does not advance a terminal attempt", () => {
    expect(() =>
      applyDestructiveVolumeAttemptEvent(
        {
          disposition: "cloud_cancelled",
          association: "associated",
          operationId: "operation-1",
          startSequence: "4",
        },
        {
          event: "core_completed",
          operationId: "operation-1",
        },
      ),
    ).toThrow("already terminal");
  });

  it("distinguishes cancellation before and after Core association", () => {
    expect(
      applyDestructiveVolumeAttemptEvent(active, {
        event: "cloud_cancelled",
      }),
    ).toEqual({
      disposition: "cloud_cancelled",
      association: "unassociated",
    });

    expect(
      applyDestructiveVolumeAttemptEvent(
        {
          disposition: "accepted",
          operationId: "operation-1",
          startSequence: "4",
        },
        {
          event: "cloud_cancelled",
          operationId: "operation-1",
        },
      ),
    ).toEqual({
      disposition: "cloud_cancelled",
      association: "associated",
      operationId: "operation-1",
      startSequence: "4",
    });
  });

  it("returns typed Effects for invalid cancellation evidence", async () => {
    const missingCoreIdentity = await Effect.runPromise(
      normalizeDestructiveVolumeAttemptEvent(
        { disposition: "accepted" },
        { event: "cloud_cancelled" },
      ).pipe(Effect.exit),
    );
    const unexpectedCoreIdentity = await Effect.runPromise(
      normalizeDestructiveVolumeAttemptEvent(
        { disposition: "active" },
        { event: "cloud_cancelled", operationId: "operation-1" },
      ).pipe(Effect.exit),
    );

    expect(Exit.isFailure(missingCoreIdentity)).toBe(true);
    expect(Exit.isFailure(unexpectedCoreIdentity)).toBe(true);
    if (Exit.isFailure(missingCoreIdentity)) {
      expect(missingCoreIdentity.cause.toString()).toContain(
        "DestructiveVolumeConflict",
      );
    }
  });

  it("projects typed Core terminal evidence in one canonical place", () => {
    const failed = {
      event: "volume_remove_failed" as const,
      failure: { kind: "dataset_busy" },
    };

    expect(isDestructiveVolumeTerminalEvidenceEvent(failed)).toBe(true);
    expect(
      projectDestructiveVolumeTerminalAttemptEvent(
        "operation-1",
        failed,
      ),
    ).toEqual({
      event: "core_terminal",
      operationId: "operation-1",
      terminalEvent: failed,
    });
    expect(
      isDestructiveVolumeTerminalEvidenceEvent({
        event: "volume_remove_running",
        stage: "destroying",
      }),
    ).toBe(false);
  });

  it("requires complete Core association before offering associated retry modes", () => {
    expect(
      destructiveVolumeRetryModeForAttempt({
        disposition: "cloud_timeout",
        operationId: "operation-1",
        startSequence: null,
      }),
    ).toBeNull();
    expect(
      destructiveVolumeRetryModeForAttempt({
        disposition: "cloud_cancelled",
        operationId: "operation-1",
        startSequence: null,
      }),
    ).toBeNull();
    expect(
      destructiveVolumeRetryModeForAttempt({
        disposition: "cloud_timeout",
        operationId: "operation-1",
        startSequence: "4",
      }),
    ).toBe("reobserve_operation");
    expect(
      destructiveVolumeRetryModeForAttempt({
        disposition: "partial",
        operationId: "operation-1",
        startSequence: "4",
      }),
    ).toBe("reconcile_tombstone");
  });

  it("does not resubmit after unknown Core recovery transport uncertainty", () => {
    expect(
      destructiveVolumeRetryModeForAttempt({
        disposition: "failed",
        operationId: null,
        startSequence: null,
        failure: {
          code: "recovery_uncertain",
          message: "Core recovery transport was uncertain.",
        },
      }),
    ).toBeNull();
    expect(
      destructiveVolumeRetryModeForAttempt({
        disposition: "failed",
        operationId: null,
        startSequence: null,
        failure: { code: "retry_exhausted", message: "Submission failed." },
      }),
    ).toBe("new_removal");
  });

  it("projects only canonical workflow ownership states", () => {
    const source = workflowAttemptSource();
    expect(projectDestructiveVolumeWorkflowAttempt(source)).toMatchObject({
      state: "new",
      id: "attempt-1",
    });
    expect(
      projectDestructiveVolumeWorkflowAttempt({
        ...source,
        inngestRunId: "run-1",
        deadlineAt: "2026-07-17T00:10:00.000Z",
      }),
    ).toMatchObject({ state: "owned_unassociated", inngestRunId: "run-1" });
    expect(
      projectDestructiveVolumeWorkflowAttempt({
        ...source,
        disposition: "accepted",
        operationId: "operation-1",
        startSequence: "4",
      }),
    ).toMatchObject({
      state: "unclaimed_associated",
      operationId: "operation-1",
    });
    expect(() =>
      projectDestructiveVolumeWorkflowAttempt({
        ...source,
        inngestRunId: "run-1",
      }),
    ).toThrow("incomplete workflow ownership");
  });

  it("compares the complete reviewed target and evidence structurally", () => {
    const source = workflowAttemptSource();
    expect(sameReviewedDestructiveVolumeIdentity(source, source)).toBe(true);
    expect(
      sameReviewedDestructiveVolumeIdentity(source, {
        target: {
          machineId: source.target.machineId,
          volumeName: source.target.volumeName,
          namespaceId: source.target.namespaceId,
          resourceId: source.target.resourceId,
          version: source.target.version,
        },
        evidence: {
          evidence: {
            referencingServices: [],
            availability: { status: "no_answer" },
            kind: { kind: "plain" },
            machineId: source.evidence.evidence.machineId,
            volumeName: source.evidence.evidence.volumeName,
            namespaceId: source.evidence.evidence.namespaceId,
          },
          reviewedAt: source.evidence.reviewedAt,
          fingerprint: source.evidence.fingerprint,
          version: source.evidence.version,
        },
      }),
    ).toBe(true);
    expect(
      sameReviewedDestructiveVolumeIdentity(source, {
        ...source,
        evidence: { ...source.evidence, reviewedAt: "2026-07-17T00:01:00Z" },
      }),
    ).toBe(false);
  });
});

function workflowAttemptSource() {
  return {
    id: "attempt-1",
    disposition: "active" as const,
    operationId: null,
    startSequence: null,
    inngestRunId: null,
    deadlineAt: null,
    target: {
      version: 1 as const,
      resourceId: "resource-1",
      namespaceId: "namespace-1",
      volumeName: "volume-1",
      machineId: "machine-1",
    },
    evidence: {
      version: 1 as const,
      fingerprint: "fingerprint-1",
      reviewedAt: "2026-07-17T00:00:00Z",
      evidence: {
        namespaceId: "namespace-1",
        volumeName: "volume-1",
        machineId: "machine-1",
        kind: { kind: "plain" as const },
        availability: { status: "no_answer" as const },
        referencingServices: [],
      },
    },
  };
}
