import { Effect, Schema } from "effect";
import { Uuid } from "#/lib/schema";
import { NewVolumeRun, volumeRunInput } from "#/modules/operations/cli-wire";
import { OPERATIONS } from "#/modules/operations/operations";
import { runVolume, unheld } from "#/modules/operations/operations.server";
import { deleteLeftover, listLeftovers } from "#/modules/volume-run/volume-run.leftovers.server";
import { getVolumeRun, listVolumeRuns } from "#/modules/volume-run/volume-run.rows.server";
import { approvalCarried, decodeBody, gated, keyedCall, missingRefusal, refusedMissing, type CliRoutes } from "#/modules/commands/cli-call.server";
import { cliRefusalResponse } from "#/server/public-error";

const LeftoverRm = Schema.Struct({ server: Schema.NonEmptyString, confirm: Schema.optional(Schema.String) });

/** Running a command against a Volume, reading the runs, and the copies deleted Volumes left. */
export const volumeRunCliRoutes = {
  "GET volume-leftovers": ({ caller }) =>
    listLeftovers(caller.organization.id).pipe(Effect.map((leftovers) => ({ leftovers }))),
  "POST volume-leftovers/:id/rm": ({ caller, request, id }) =>
    Effect.gen(function* () {
      // A deleted Volume has no Environment left to scope a token to, so only a full grant deletes its copies.
      const lacking = unheld(caller, OPERATIONS["volume.remove_mirror"].requires, null);
      if (lacking !== null) return cliRefusalResponse(lacking.refusal);
      const carried = approvalCarried(request);
      if (carried !== null) return carried;
      const body = yield* decodeBody(LeftoverRm, request, "Deleting a leftover copy takes its Server.");
      const input = { dockerVolume: decodeURIComponent(id ?? ""), server: body.server, confirm: body.confirm ?? null };
      const deleted = yield* deleteLeftover({ userId: caller.userId, organizationId: caller.organization.id }, input, gated(caller, request));
      return deleted.ok ? { run: deleted.run } : cliRefusalResponse(deleted.refusal);
    }),
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
