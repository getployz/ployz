import "@tanstack/react-start/server-only";
import { sendInngestEvent } from "#/modules/inngest/client";
import { Effect } from "effect";
import { DestructiveVolumeProviderFailure } from "#/modules/operations/destructive-volume-errors";
import { acknowledgeDestructiveVolumeRequest } from "#/modules/operations/destructive-volume-attempt.repository";
import { destructiveVolumeRequestedEvent } from "#/modules/operations/destructive-volume-outbox";

export const dispatchDestructiveVolumeAttempt = Effect.fn(
  "Operations.dispatchDestructiveVolumeAttempt",
)(function* (attemptId: string) {
  yield* sendInngestEvent(destructiveVolumeRequestedEvent(attemptId)).pipe(
    Effect.mapError(
      (cause) =>
      new DestructiveVolumeProviderFailure({ cause }),
    ),
  );
  return yield* acknowledgeDestructiveVolumeRequest({
    attemptId,
    publishedAt: new Date(),
  });
});
