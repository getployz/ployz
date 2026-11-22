import { Effect, Schema } from "effect";
import { Uuid } from "#/lib/schema";
import { NewVolumeRun, volumeRunInput } from "#/modules/operations/cli-wire";
import { runVolume } from "#/modules/operations/operations.server";
import { getVolumeRun, listVolumeRuns } from "#/modules/volume-run/volume-run.rows.server";
import { decodeBody, keyedCall, missingRefusal, refusedMissing, type CliRoutes } from "#/modules/commands/cli-call.server";
import { cliRefusalResponse } from "#/server/public-error";

/** Running a command against a Volume, and reading the runs. */
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
