import "@tanstack/react-start/server-only";
import { Effect, Schema } from "effect";
import { refused, StoreRefused } from "#/lib/refusal";
import { defineCommand } from "#/modules/commands/define";
import { OPERATIONS } from "#/modules/operations/operations";
import { PRIVATE_ACCESS_WANTS } from "#/modules/private-access/private-access";
import { privateAccessAsked, setPrivateAccess } from "#/modules/private-access/private-access.server";
import type { OrganizationRuntime } from "#/modules/runtime/organization-runtime.server";
import { refusalFor } from "#/modules/organization/permissions";

const use = OPERATIONS["private_access.use"];

const withRuntime = <A, E, R>(effect: Effect.Effect<A, E, R>): Effect.Effect<A, E, R | OrganizationRuntime> => effect;

const To = Schema.Literals(PRIVATE_ACCESS_WANTS).annotate({
  description: "Use off. Turn it on in Ployz Cloud.",
});

/** Turning it on takes a Tailscale auth key, which only Ployz Cloud's page seals for the Servers. */
const ON_IN_CLOUD = "Turn it on in Ployz Cloud.";

export const privateAccessUse = defineCommand({
  name: "private_access_use",
  requires: use.requires,
  path: ["cluster", "private-access"],
  about: "Turn Private Access over Tailscale off. Turn it on in Ployz Cloud.",
  title: "Turn Private Access off",
  examples: ["ployz cluster private-access off"],
  kind: "write",
  removes: false,
  // Off turns off what is already off.
  idempotent: true,
  gate: {
    gate: "ask",
    asked: (caller, input) => input.to === "on"
      ? Effect.fail(new StoreRefused({ code: "failed_precondition", message: ON_IN_CLOUD, details: null }))
      : Effect.succeed(privateAccessAsked(caller.organization.id, input.to)),
  },
  agents: true,
  openWorld: false,
  typed: null,
  positional: ["to"],
  input: Schema.Struct({ to: To }),
  output: Schema.Struct({ to: To, routers: Schema.Number }),
  render: "message",
  run: (caller, input) => caller.support !== null
    ? Effect.succeed(refused("forbidden", refusalFor(use.requires)))
    : withRuntime(setPrivateAccess(caller.organization.id, null)).pipe(
      Effect.map(({ failures }) => {
        if (failures.length > 0) return refused("unavailable", failures.map(({ name, message }) => `${name}: ${message}`).join(" "));
        return { ok: true, output: { to: input.to, routers: 0 }, message: "Private Access is off." } as const;
      }),
      Effect.catchTag("Validation", (problem) => Effect.succeed(refused("failed_precondition", problem.message))),
    ),
});
