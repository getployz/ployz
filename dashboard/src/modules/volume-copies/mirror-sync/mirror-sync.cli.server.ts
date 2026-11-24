import { Effect } from "effect";
import { SyncVolume } from "#/modules/operations/cli-wire";
import { OPERATIONS } from "#/modules/operations/operations";
import { unheld } from "#/modules/operations/operations.server";
import { refreshMirror } from "#/modules/volume-copies/mirror-sync/mirror-sync.refresh.server";
import { approvalCarried, decodeBody, refusedMissing, type CliRoutes } from "#/modules/commands/cli-call.server";
import { cliRefusalResponse } from "#/server/public-error";

/** Syncing a Volume's mirror. */
export const mirrorSyncCliRoutes = {
  "POST volumes/:id/sync": ({ caller, request, id }) =>
    Effect.gen(function* () {
      const body = yield* decodeBody(SyncVolume, request, "A volume sync takes its environment.");
      const unsynced = unheld(caller, OPERATIONS["volume.sync"].requires, body.environment.environment);
      if (unsynced !== null) return cliRefusalResponse(unsynced.refusal);
      const carried = approvalCarried(request);
      if (carried !== null) return carried;
      return yield* refreshMirror({ userId: caller.userId, organizationId: caller.organization.id }, body.environment, id ?? "").pipe(
        Effect.map((synced) => synced.ok ? { lines: synced.lines } : cliRefusalResponse(synced.refusal)),
        Effect.catchTag("NotFound", refusedMissing),
      );
    }),
} satisfies CliRoutes;
