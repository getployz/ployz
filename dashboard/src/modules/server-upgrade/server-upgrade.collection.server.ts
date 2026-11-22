import "@tanstack/react-start/server-only";
import { Effect } from "effect";
import { serverUpgradeSettings } from "./server-upgrade.collection";
import { organizationServerUpgrades } from "./tables";
import { defineProjection } from "#/collections/change-log/projection.server";
import { Database } from "#/server/database.server";

export const serverUpgradeSettingsProjection = defineProjection(serverUpgradeSettings, (scoped) => Effect.gen(function* () {
  const database = yield* Database;
  const settings = organizationServerUpgrades;
  return yield* database.drizzle.select({
    id: settings.organizationId, automatic: settings.automatic, channel: settings.channel, approval: settings.channelApproval,
  }).from(settings).where(scoped(settings));
}));
