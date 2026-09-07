import type { PersistedOperationEvent } from "#/modules/operations/core-operation-evidence.server";
import { asRecord, asString } from "#/lib/json";

export type DestructiveVolumeEvidenceEvent =
  | { event: "volume_remove_submitted" }
  | { event: "volume_remove_running"; stage: string }
  | { event: "volume_remove_completed" }
  | { event: "volume_remove_failed"; failure: { kind: string } }
  | { event: "operation_interrupted"; evidence: unknown }
  | { event: "cancelled"; reason: "[redacted]" };

export function destructiveVolumeEventFromPersisted(
  persisted: PersistedOperationEvent,
): DestructiveVolumeEvidenceEvent | null {
  const payload = persisted.payload;
  switch (persisted.eventType) {
    case "volume_remove_submitted":
    case "volume_remove_completed":
      return { event: persisted.eventType };
    case "volume_remove_running": {
      const stage = asString(payload["stage"]);
      return stage === null
        ? null
        : { event: persisted.eventType, stage };
    }
    case "volume_remove_failed": {
      const failure = asRecord(payload["failure"]);
      const kind = failure === null ? null : asString(failure["kind"]);
      return kind === null
        ? null
        : {
            event: persisted.eventType,
            failure: { kind },
          };
    }
    case "operation_interrupted":
      return payload["evidence"]
        ? { event: persisted.eventType, evidence: payload["evidence"] }
        : null;
    case "cancelled":
      return payload["reason"] === "[redacted]"
        ? { event: persisted.eventType, reason: "[redacted]" }
        : null;
    default:
      return null;
  }
}
