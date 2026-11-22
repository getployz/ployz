import type { ServerUpgradeSettingsRow } from "./server-upgrade";
import { defineOrgCollection } from "#/collections/change-log/declare";

/** The Organization's Server upgrade settings: no row reads as the defaults. */
export const serverUpgradeSettings = defineOrgCollection({
  name: "organization_server_upgrades",
  sources: ["organization_server_upgrades"],
  key: (row: ServerUpgradeSettingsRow) => row.id,
});
