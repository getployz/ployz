import "@tanstack/react-start/server-only";
import type { RuntimeWatchView } from "@ployz/sdk";
import { Effect } from "effect";
import { InngestClient } from "#/modules/inngest/client";
import { recordMachineNames } from "#/modules/machines/machine-names.server";
import { requestSealingKeyEnsure } from "#/modules/machines/sealing-ensure.server";
import type { FrameRecord } from "#/modules/runtime/organization-runtime.server";
import { Database } from "#/server/database.server";
import { SecretEncryption } from "#/utils/encrypted-secret.server";

const SEALING_ENSURE_TIMEOUT = "10 seconds";

export const runtimeFrameDigest = (frame: RuntimeWatchView) =>
  JSON.stringify(frame.machines.map(({ machine }) => [machine.id, machine.name, machine.runtime.sealing?.kind ?? null]));

export const runtimeFrameRecord = Effect.gen(function* () {
  const database = yield* Database;
  const encryption = yield* SecretEncryption;
  const inngest = yield* InngestClient;
  return {
    digest: runtimeFrameDigest,
    write: (organizationId, frame) => recordMachineNames(organizationId, frame).pipe(
      Effect.andThen(requestSealingKeyEnsure(organizationId, frame).pipe(
        // The recorder writes uninterruptibly for its SQL; a stalled send must not hold the Organization's writer.
        Effect.timeoutOrElse({
          duration: SEALING_ENSURE_TIMEOUT,
          orElse: () => Effect.fail(new Error("Asking for the Sealing Key ensure timed out")),
        }),
        Effect.interruptible,
      )),
      Effect.provideService(Database, database),
      Effect.provideService(SecretEncryption, encryption),
      Effect.provideService(InngestClient, inngest),
    ),
  } satisfies FrameRecord;
});
