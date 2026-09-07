import "@tanstack/react-start/server-only";
import { Effect } from "effect";
import { SdkSurfaceNotShipped } from "#/modules/runtime/ployz.server";
import type { ReviewedDestructiveVolumeTarget } from "#/modules/operations/destructive-volume-attempt";
import type { ReviewedDestructiveVolumeEvidence } from "#/modules/operations/destructive-volume-attempt";
import type { DestructiveVolumeEvidenceEvent } from "#/modules/operations/destructive-volume-operation-evidence";

function volumeRemoveNotShipped() {
  return new SdkSurfaceNotShipped({
    surface: "removeVolumes",
    ticket: "getployz/ployz2#352",
  });
}

export function verifyFreshDestructiveVolumeEvidence(_input: {
  target: ReviewedDestructiveVolumeTarget;
  evidence: ReviewedDestructiveVolumeEvidence;
}): Effect.Effect<
  { state: "matched" } | { state: "changed"; message: string },
  SdkSurfaceNotShipped
> {
  return Effect.fail(volumeRemoveNotShipped());
}

export function recoverDestructiveVolumeAcceptance(_input: {
  attemptId: string;
  target: ReviewedDestructiveVolumeTarget;
}): Effect.Effect<
  | { state: "absent" }
  | { state: "accepted"; operation_id: string; start_sequence: string },
  SdkSurfaceNotShipped
> {
  return Effect.fail(volumeRemoveNotShipped());
}

export function submitDestructiveVolume(_input: {
  attemptId: string;
  target: ReviewedDestructiveVolumeTarget;
}): Effect.Effect<
  { operation_id: string; start_sequence: string },
  SdkSurfaceNotShipped
> {
  return Effect.fail(volumeRemoveNotShipped());
}

export function watchDestructiveVolumeBatch(_input: {
  organizationId: string;
  operationId: string;
  maxPages: number;
}): Effect.Effect<
  {
    state: "more" | "caught_up" | "terminal";
    events: DestructiveVolumeEvidenceEvent[];
  },
  SdkSurfaceNotShipped
> {
  return Effect.fail(volumeRemoveNotShipped());
}
