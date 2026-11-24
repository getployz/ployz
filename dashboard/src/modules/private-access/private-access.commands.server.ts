import "@tanstack/react-start/server-only";
import { Effect, Schema } from "effect";
import { refused } from "#/lib/refusal";
import { defineCommand } from "#/modules/commands/define";
import { OPERATIONS } from "#/modules/operations/operations";
import { PRIVATE_ACCESS_WANTS } from "#/modules/private-access/private-access";

const use = OPERATIONS["private_access.use"];

/** Turning it on takes a Tailscale auth key only Ployz Cloud's page seals, and a person turns it either way there. */
const IN_CLOUD = "Turn Private Access on or off in Ployz Cloud, under Integrations.";

const To = Schema.Literals(PRIVATE_ACCESS_WANTS).annotate({ description: IN_CLOUD });

export const privateAccessUse = defineCommand({
  name: "private_access_use",
  requires: use.requires,
  path: ["cluster", "private-access"],
  about: `Private Access isn't turned on or off here. ${IN_CLOUD}`,
  title: "Turn Private Access on or off",
  examples: ["ployz cluster private-access off"],
  kind: "read",
  agents: true,
  openWorld: false,
  typed: null,
  positional: ["to"],
  input: Schema.Struct({ to: To }),
  output: Schema.Struct({ to: To, routers: Schema.Number }),
  render: "message",
  run: () => Effect.succeed(refused("failed_precondition", IN_CLOUD)),
});
