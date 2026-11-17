import "@tanstack/react-start/server-only";
import { Effect, Schema } from "effect";
import { defineCommand } from "#/modules/commands/command.server";
import { RELEASE_CHANNELS } from "#/modules/server-upgrade/server-upgrade";
import { organizationServerUpgradeSettings, setServerUpgradeSettings } from "#/modules/server-upgrade/server-upgrade.server";

const UpgradePolicy = Schema.Struct({ channel: Schema.Literals(RELEASE_CHANNELS), automatic: Schema.Boolean });

// `ServerUpgradeSettingsChange` with each field worded for a caller choosing it; `run` hands it to
// `setServerUpgradeSettings`, so a field that stops fitting that schema fails to compile.
const PolicyChange = Schema.Struct({
  channel: Schema.optionalKey(Schema.Literals(RELEASE_CHANNELS).annotate({ description: "The Release Channel every Upgrade follows from now on: stable or beta. Leave it out to keep the current one" })),
  automatic: Schema.optionalKey(Schema.Boolean.annotate({ description: "true lets Servers behind the channel upgrade on their own, false stops that. Leave it out to keep it as it is" })),
});

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
  input: PolicyChange,
  output: UpgradePolicy,
  render: "record",
  run: (caller, change) => setServerUpgradeSettings(caller.organization.id, change).pipe(
    Effect.map(({ channel, automatic }) => ({ ok: true, output: { channel, automatic } }) as const),
  ),
});
