import { createServerFn } from "@tanstack/react-start";
import { Schema } from "effect";
import {
  ChannelPointer,
  RequestServerUpgradeInput,
  SetServerUpgradeSettingsInput,
} from "#/modules/server-upgrade/server-upgrade";
import {
  channelRelease,
  listLatestServerUpgrades,
  requestServerUpgrade,
  setServerUpgradeSettings,
} from "#/modules/server-upgrade/server-upgrade.server";
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
  .handler(({ context, data }) => runActor(context, setServerUpgradeSettings(context.actor, data)));
