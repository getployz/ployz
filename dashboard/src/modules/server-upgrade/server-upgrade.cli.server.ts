import { Effect } from "effect";
import { UpgradeServer } from "#/modules/operations/cli-wire";
import { upgradeServer } from "#/modules/operations/operations.server";
import { readCliServerUpgrade } from "#/modules/server-upgrade/server-upgrade.server";
import { accepted, admitted, decodeBody, keyedCall, missingRefusal, noSuchServer, serverIdOf, type CliRoutes } from "#/modules/commands/cli-call.server";

const isUpgradeAttemptId = (id: string | undefined): id is string => id !== undefined && /^[0-9a-f]{32}$/.test(id);

/** Upgrading a Server, and reading how the upgrade went. */
export const serverUpgradeCliRoutes = {
  "POST servers/:id/upgrade": ({ caller, request, id }) =>
    Effect.gen(function* () {
      const machineId = serverIdOf(id);
      if (machineId === null) return noSuchServer();
      const input = yield* decodeBody(UpgradeServer, request, "An upgrade takes at most the Release Channel it names.");
      return yield* keyedCall(request, (call) =>
        Effect.map(upgradeServer(caller, { machineId, channel: input.channel }, call, input.server_name), (admission) =>
          admitted(admission, accepted)),
      );
    }),
  "GET server-upgrades/:id": ({ caller, id }) =>
    isUpgradeAttemptId(id) ? readCliServerUpgrade(caller.organization.id, id) : Effect.succeed(missingRefusal("No such upgrade.")),
} satisfies CliRoutes;
