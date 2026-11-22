import "@tanstack/react-start/server-only";
import { Effect, Schema } from "effect";
import { defineCommand } from "#/modules/commands/define";
import { RELEASE_CHANNELS, ServerUpgradeSettingsChange } from "#/modules/server-upgrade/server-upgrade";
import { organizationServerUpgradeSettings, setServerUpgradeSettings } from "#/modules/server-upgrade/server-upgrade.server";

const UpgradePolicy = Schema.Struct({ channel: Schema.Literals(RELEASE_CHANNELS), automatic: Schema.Boolean });

export const serverUpgradePolicyShow = defineCommand({
  name: "server_upgrade_policy_show",
  requires: "servers:read",
  path: ["server", "upgrade-policy", "show"],
  about: "Show the Organization's upgrade policy: the Release Channel its Servers follow and whether they upgrade automatically",
  title: "Show the upgrade policy",
  examples: ["ployz server upgrade-policy show"],
  kind: "read",
  agents: true,
  openWorld: false,
  typed: null,
  positional: [],
  input: Schema.Struct({}),
  output: UpgradePolicy,
  render: "record",
  run: (caller) => organizationServerUpgradeSettings(caller.organization.id).pipe(
    Effect.map(({ channel, automatic }) => ({ ok: true, output: { channel, automatic } }) as const),
  ),
});

export const serverUpgradePolicySet = defineCommand({
  name: "server_upgrade_policy_set",
  requires: "servers:operate",
  path: ["server", "upgrade-policy", "set"],
  about: "Change the Organization's upgrade policy: its Release Channel, automatic upgrades on or off, or both. It takes effect at once, and turning automatic on starts a Rollout over the Servers that are behind",
  title: "Change the upgrade policy",
  examples: ["ployz server upgrade-policy set --channel beta", "ployz server upgrade-policy set --automatic true"],
  kind: "write",
  removes: false,
  // The same settings saved again are the same row; a second automatic Rollout finds no Server behind.
  idempotent: true,
  gate: "never",
  agents: true,
  openWorld: false,
  typed: null,
  positional: [],
  input: ServerUpgradeSettingsChange,
  output: UpgradePolicy,
  render: "record",
  run: (caller, change) => setServerUpgradeSettings(caller.organization.id, change).pipe(
    Effect.map(({ channel, automatic }) => ({ ok: true, output: { channel, automatic } }) as const),
  ),
});
