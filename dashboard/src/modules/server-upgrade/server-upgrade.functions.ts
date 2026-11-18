import { createServerFn } from "@tanstack/react-start";
import { getRequest } from "@tanstack/react-start/server";
import { Effect, Schema } from "effect";
import { runCommand } from "#/modules/commands/commands.server";
import { dashboardCaller } from "#/modules/identity/caller.server";
import {
  ChannelPointer,
  RequestServerUpgradeInput,
  SetServerUpgradeSettingsInput,
} from "#/modules/server-upgrade/server-upgrade";
import {
  channelRelease,
  listLatestServerUpgrades,
  requestServerUpgrade,
} from "#/modules/server-upgrade/server-upgrade.server";
import { Validation } from "#/server/public-error";
import { actorMiddleware, publicErrorMiddleware, runActor, strictValidator } from "#/server/tanstack";

export const listLatestServerUpgradesServerFn = createServerFn({ method: "GET" })
  .middleware([publicErrorMiddleware, actorMiddleware])
  .validator(strictValidator(Schema.Struct({ organizationSlug: Schema.NonEmptyString })))
  .handler(({ context, data }) => runActor(context, listLatestServerUpgrades(context.actor, data)));

/** A null line reads the unscoped stable pointer, only for the new-major-line notice. */
export const readChannelReleaseServerFn = createServerFn({ method: "GET" })
  .middleware([publicErrorMiddleware, actorMiddleware])
  .validator(strictValidator(ChannelPointer))
  .handler(({ context, data }) => runActor(context, channelRelease(data.channel, data.line)));

export const requestServerUpgradeServerFn = createServerFn({ method: "POST" })
  .middleware([publicErrorMiddleware, actorMiddleware])
  .validator(strictValidator(RequestServerUpgradeInput))
  .handler(({ context, data }) => runActor(context, requestServerUpgrade(context.actor, data)));

export const setServerUpgradeSettingsServerFn = createServerFn({ method: "POST" })
  .middleware([publicErrorMiddleware, actorMiddleware])
  .validator(strictValidator(SetServerUpgradeSettingsInput))
  .handler(({ context, data: { organizationSlug, ...change } }) => runActor(context, Effect.gen(function* () {
    const caller = yield* dashboardCaller(getRequest().headers, organizationSlug);
    const ran = yield* runCommand(caller, "server_upgrade_policy_set", change, { approvalId: null, key: null });
    if (!ran.ok) return yield* new Validation({ message: ran.refusal.message, userFacing: true });
    return { id: caller.organization.id, ...ran.output };
  })));
