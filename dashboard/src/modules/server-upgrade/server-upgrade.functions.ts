import { createServerFn } from "@tanstack/react-start";
import { Schema } from "effect";
import { ReleaseLine, RequestServerUpgradeInput, SetAutomaticServerUpgradesInput } from "#/modules/server-upgrade/server-upgrade";
import {
  listLatestServerUpgrades,
  requestServerUpgrade,
  setAutomaticServerUpgrades,
  stableRelease,
} from "#/modules/server-upgrade/server-upgrade.server";
import { actorMiddleware, publicErrorMiddleware, runActor, strictValidator } from "#/server/tanstack";

export const listLatestServerUpgradesServerFn = createServerFn({ method: "GET" })
  .middleware([publicErrorMiddleware, actorMiddleware])
  .validator(strictValidator(Schema.Struct({ organizationSlug: Schema.NonEmptyString })))
  .handler(({ context, data }) => runActor(context, listLatestServerUpgrades(context.actor, data)));

export const readStableReleaseServerFn = createServerFn({ method: "GET" })
  .middleware([publicErrorMiddleware, actorMiddleware])
  .validator(strictValidator(Schema.Struct({ line: ReleaseLine })))
  .handler(({ context, data }) => runActor(context, stableRelease(data.line)));

export const requestServerUpgradeServerFn = createServerFn({ method: "POST" })
  .middleware([publicErrorMiddleware, actorMiddleware])
  .validator(strictValidator(RequestServerUpgradeInput))
  .handler(({ context, data }) => runActor(context, requestServerUpgrade(context.actor, data)));

export const setAutomaticServerUpgradesServerFn = createServerFn({ method: "POST" })
  .middleware([publicErrorMiddleware, actorMiddleware])
  .validator(strictValidator(SetAutomaticServerUpgradesInput))
  .handler(({ context, data }) => runActor(context, setAutomaticServerUpgrades(context.actor, data)));
