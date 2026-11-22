import { Effect, Schema } from "effect";
import { Uuid } from "#/lib/schema";
import { NewVolumeRun, SyncVolume, volumeRunInput } from "#/modules/operations/cli-wire";
import { OPERATIONS } from "#/modules/operations/operations";
import { runVolume, unheld } from "#/modules/operations/operations.server";
import { refreshMirror } from "#/modules/volume-copies/mirror-sync/mirror-sync.refresh.server";
import { getVolumeRun, listVolumeRuns } from "#/modules/volume-run/volume-run.rows.server";
import { approvalCarried, decodeBody, keyedCall, missingRefusal, refusedMissing, type CliRoutes } from "#/modules/commands/cli-call.server";
import { cliRefusalResponse } from "#/server/public-error";

/** Running a command against a Volume, syncing its mirror, and reading the runs. */
export const volumeRunCliRoutes = {
  "POST volumes/:id/runs": ({ caller, request, id }) =>
    Effect.gen(function* () {
      const body = yield* decodeBody(NewVolumeRun, request, "A volume run takes its environment and what to run.");
      return yield* keyedCall(request, (call) => {
        const input = { volumeId: id ?? "", environment: body.environment, ...volumeRunInput(body) };
        return Effect.gen(function* () {
          const run = yield* runVolume(caller, input, call);
          return run.ok ? { run: yield* getVolumeRun(caller.organization.id, run.id) } : cliRefusalResponse(run.refusal);
        }).pipe(Effect.catchTag("NotFound", refusedMissing));
      });
    }),
  "POST volumes/:id/sync": ({ caller, request, id }) =>
    Effect.gen(function* () {
      const body = yield* decodeBody(SyncVolume, request, "A volume sync takes its environment.");
      const unsynced = unheld(caller, OPERATIONS["volume.sync"].requires, body.environment.environment);
      if (unsynced !== null) return cliRefusalResponse(unsynced.refusal);
      // It asks no one, and still refuses an approval carried into it that isn't for exactly this sync.
      const carried = yield* approvalCarried(caller, request, { command: "sync", subject: `volume:${id ?? ""}` });
      if (carried !== null) return carried;
      return yield* refreshMirror({ userId: caller.userId, organizationId: caller.organization.id }, body.environment, id ?? "").pipe(
        Effect.map((synced) => synced.ok ? { lines: synced.lines } : cliRefusalResponse(synced.refusal)),
        Effect.catchTag("NotFound", refusedMissing),
      );
    }),
  "GET volumes/:id/runs": ({ caller, id }) =>
    Effect.gen(function* () {
      return { runs: yield* listVolumeRuns(caller.organization.id, id ?? "") };
    }),
  "GET volume-runs/:id": ({ caller, id }) =>
    Schema.is(Uuid)(id)
      ? getVolumeRun(caller.organization.id, id).pipe(
        Effect.map((run) => ({ run })),
        Effect.catchTag("NotFound", refusedMissing),
      )
      : Effect.succeed(missingRefusal("No such volume run.")),
} satisfies CliRoutes;
