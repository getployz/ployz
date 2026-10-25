import "@tanstack/react-start/server-only";
import type { RuntimeWatchView } from "@ployz/sdk";
import { Effect } from "effect";
import { InngestClient } from "#/modules/inngest/client";
import { recordMachineNames } from "#/modules/machines/machine-names.server";
import { requestSealingKeyEnsure } from "#/modules/machines/sealing-ensure.server";
import type { FrameRecord } from "#/modules/runtime/organization-runtime.server";
import { Database } from "#/server/database.server";

const SEALING_ENSURE_TIMEOUT = "10 seconds";

export const runtimeNamesDigest = (frame: RuntimeWatchView) =>
  JSON.stringify(frame.machines.map(({ machine }) => [machine.id, machine.name]));

export const runtimeSealingDigest = (frame: RuntimeWatchView) =>
  JSON.stringify(frame.machines.map(({ machine }) => [machine.id, machine.runtime.sealing?.kind ?? null]));

/** The Machine names a frame reports, then the Sealing Key ensure its holders call for; each remembered apart. */
export const runtimeFrameRecords = Effect.gen(function* () {
  const database = yield* Database;
  const inngest = yield* InngestClient;
  const names: FrameRecord = {
    digest: runtimeNamesDigest,
    write: (organizationId, _generation, frame) => recordMachineNames(organizationId, frame).pipe(
      Effect.as("settled" as const),
      Effect.provideService(Database, database),
    ),
  };
  const sealing: FrameRecord = {
    digest: runtimeSealingDigest,
    write: (organizationId, generation, frame) => requestSealingKeyEnsure(organizationId, generation, frame).pipe(
      // The recorder writes uninterruptibly for its SQL; a stalled send must not hold the Organization's writer.
      Effect.timeoutOrElse({
        duration: SEALING_ENSURE_TIMEOUT,
        orElse: () => Effect.fail(new Error("Asking for the Sealing Key ensure timed out")),
      }),
      Effect.interruptible,
      // An ensure that failed, in its key read or its send, is asked again after the recorder's wait, not on every frame.
      Effect.catch((error) => Effect.logWarning("Sealing Key ensure not asked", error).pipe(Effect.as("asked" as const))),
      Effect.provideService(Database, database),
      Effect.provideService(InngestClient, inngest),
    ),
  };
  return [names, sealing] as const;
});
