import { InngestTestEngine } from "@inngest/test";
import { Effect } from "effect";
import { Inngest } from "inngest";
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { DestructiveVolumeAttemptRecord } from "#/modules/operations/destructive-volume-attempt.repository";
import * as attemptRepository from "#/modules/operations/destructive-volume-attempt.repository";
import { createRecoverAbandonedDestructiveVolumeAttempts } from "./abandoned-owner-recovery";

vi.spyOn(
  attemptRepository,
  "listOwnedDestructiveVolumeAttemptsPage",
).mockImplementation(() => Effect.succeed([]));
vi.spyOn(
  attemptRepository,
  "finalizeUnassociatedDestructiveVolumeAttempt",
).mockImplementation(({ attemptId }) =>
  Effect.succeed({ state: "recorded", attempt: attemptRecord(attemptId) }),
);

describe("abandoned destructive volume recovery Inngest adapter", () => {
  beforeEach(() => {
    vi.clearAllMocks();
  });

  it("owns the exact schedule, retry, and global concurrency policy", () => {
    const fn = createRecoverAbandonedDestructiveVolumeAttempts(
      new Inngest({ id: "test" }),
    );

    expect(fn.opts).toEqual(
      expect.objectContaining({
        id: "recover-abandoned-destructive-volume-attempts",
        retries: 3,
        triggers: [{ cron: "* * * * *" }],
        concurrency: [{ limit: 1 }],
      }),
    );
  });

  it("runs recovery as explicit durable steps and reports the owned outcome", async () => {
    vi.mocked(
      attemptRepository.listOwnedDestructiveVolumeAttemptsPage,
    ).mockReturnValueOnce(
      Effect.succeed([
        {
          organizationId: "organization-1",
          deadlineAt: new Date("2026-07-10T12:00:00.000Z"),
          attempt: attemptRecord("attempt-1"),
        },
      ]),
    );
    const engine = new InngestTestEngine({
      function: createRecoverAbandonedDestructiveVolumeAttempts(
        new Inngest({ id: "test" }),
      ),
    });

    const output = await engine.execute();

    expect(output.result).toEqual({ recovered: 1 });
    expect(output.ctx.step.run).toHaveBeenCalledWith(
      "list-abandoned-destructive-volume-first",
      expect.any(Function),
    );
    expect(output.ctx.step.run).toHaveBeenCalledWith(
      "fail-stale-destructive-volume-attempt-1",
      expect.any(Function),
    );
  });
});

function attemptRecord(
  id: string,
): DestructiveVolumeAttemptRecord & { organizationId: string } {
  return {
    id,
    organizationId: "organization-1",
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
    disposition: "active",
    operationId: null,
    startSequence: null,
    inngestRunId: `run-${id}`,
    requestPublishedAt: null,
    acceptedAt: null,
    terminalEvent: null,
    failure: null,
    deadlineAt: new Date("2026-07-10T12:00:00.000Z"),
    terminalAt: null,
    createdAt: new Date("2026-07-10T11:00:00.000Z"),
    updatedAt: new Date("2026-07-10T11:00:00.000Z"),
  };
}
