import { describe, expect, it, vi } from "vitest";
import { Effect } from "effect";
import { closeDestructiveVolumeWatchAndReconcileWithDeps } from "./destructive-volume-workflow.server";

describe("destructive volume watch closure", () => {
  it("closes the watch before recording Cloud timeout", async () => {
    const order: string[] = [];
    const record = vi.fn(() => {
      order.push("record");
      return Effect.succeed({ state: "recorded" as const, attempt: {} as never });
    });
    await Effect.runPromise(closeDestructiveVolumeWatchAndReconcileWithDeps(
      {
        attemptId: "attempt-1",
        organizationId: "organization-1",
        operationId: "operation-1",
        startSequence: "4",
        expectedInngestRunId: "run-1",
        state: "cloud_timeout",
        now: new Date("2026-07-17T00:10:00Z"),
      },
      {
        closeWatch: vi.fn(() => {
          order.push("close");
          return Effect.succeed({
            state: "closed" as const,
            observationState: "cloud_timeout" as const,
            observationDetail: null,
          });
        }),
        parseTerminal: vi.fn(),
        record,
        complete: vi.fn(),
      },
    ));
    expect(order).toEqual(["close", "record"]);
    expect(record).toHaveBeenCalledWith(expect.objectContaining({
      event: {
        event: "cloud_timeout",
        operationId: "operation-1",
        startSequence: "4",
      },
    }));
  });

  it("prefers a Core terminal event won by the closure race", async () => {
    const record = vi.fn(() => Effect.succeed({ state: "recorded" as const, attempt: {} as never }));
    const complete = vi.fn(() => Effect.succeed({ state: "recorded" as const, attempt: {} as never }));
    await Effect.runPromise(closeDestructiveVolumeWatchAndReconcileWithDeps(
      {
        attemptId: "attempt-1",
        organizationId: "organization-1",
        operationId: "operation-1",
        startSequence: "4",
        expectedInngestRunId: "run-1",
        state: "cloud_cancelled",
        now: new Date(),
      },
      {
        closeWatch: vi.fn(() => Effect.succeed({
          state: "core_terminal" as const,
          terminalEvent: {
            sequence: "8",
            eventType: "volume_remove_completed",
            payload: { operationId: "operation-1" },
          },
        })),
        parseTerminal: vi.fn(() => ({ event: "volume_remove_completed" as const })),
        record,
        complete,
      },
    ));
    expect(complete).toHaveBeenCalledWith({
      attemptId: "attempt-1",
      operationId: "operation-1",
      now: expect.any(Date),
    });
    expect(record).not.toHaveBeenCalled();
  });
});
