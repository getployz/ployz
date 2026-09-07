import { describe, expect, it } from "vitest";
import { destructiveVolumeEventFromPersisted } from "./destructive-volume-operation-evidence";

describe("destructive volume operation evidence", () => {
  it("decodes persisted volume completion", () => {
    expect(
      destructiveVolumeEventFromPersisted({
        sequence: "8",
        eventType: "volume_remove_completed",
        payload: { operationId: "operation-1" },
      }),
    ).toEqual({ event: "volume_remove_completed" });
  });

  it("retains persisted interruption evidence", () => {
    const evidence = {
      cause: "core_shutdown",
      last_durable_stage: { kind: "volume_remove_accepted" },
      kind: "volume_remove",
      uncertain_work: "intent_and_runtime",
      next_action: "inspect_then_resubmit",
    };
    expect(
      destructiveVolumeEventFromPersisted({
        sequence: "9",
        eventType: "operation_interrupted",
        payload: { operationId: "operation-1", evidence },
      }),
    ).toEqual({ event: "operation_interrupted", evidence });
  });
});
