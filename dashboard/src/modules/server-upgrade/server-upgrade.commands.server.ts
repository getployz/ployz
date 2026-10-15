import "@tanstack/react-start/server-only";
import { Effect, Schema } from "effect";
import { defineCommand } from "#/modules/commands/command.server";
import { RELEASE_CHANNELS, ServerUpgradeSettingsChange } from "#/modules/server-upgrade/server-upgrade";
import { organizationServerUpgradeSettings, setServerUpgradeSettings } from "#/modules/server-upgrade/server-upgrade.server";

const UpgradePolicy = Schema.Struct({ channel: Schema.Literals(RELEASE_CHANNELS), automatic: Schema.Boolean });

export const serverUpgradePolicyShow = defineCommand({
  name: "server_upgrade_policy_show",
  path: ["server", "upgrade-policy", "show"],
  about: "Show the Organization's upgrade policy: its Release Channel and whether Servers upgrade automatically",
  examples: ["ployz server upgrade-policy show"],
  kind: "read",
  agents: true,
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
  path: ["server", "upgrade-policy", "set"],
  about: "Change the Organization's upgrade policy: the Release Channel every upgrade follows, or automatic upgrades on or off",
  examples: ["ployz server upgrade-policy set --channel beta", "ployz server upgrade-policy set --automatic true"],
  kind: "write",
  removes: false,
  gate: "never",
  agents: true,
  typed: null,
  positional: [],
  input: ServerUpgradeSettingsChange,
  output: UpgradePolicy,
  render: "record",
  run: (caller, change) => setServerUpgradeSettings(caller.organization.id, change).pipe(
    Effect.map(({ channel, automatic }) => ({ ok: true, output: { channel, automatic } }) as const),
  ),
});
