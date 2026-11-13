import "@tanstack/react-start/server-only";
import { Effect, Schema } from "effect";
import { defineCommand, refused } from "#/modules/commands/command.server";
import { OPERATIONS } from "#/modules/operations/operations";
import { PRIVATE_ACCESS_WANTS, routerCount } from "#/modules/private-access/private-access";
import { privateAccessAsked, runPrivateAccess } from "#/modules/private-access/private-access.server";
import type { OrganizationRuntime } from "#/modules/runtime/organization-runtime.server";

const use = OPERATIONS["private_access.use"];

const withRuntime = <A, E, R>(effect: Effect.Effect<A, E, R>): Effect.Effect<A, E, R | OrganizationRuntime> => effect;

const To = Schema.Literals(PRIVATE_ACCESS_WANTS).annotate({
  description: "on or off. On waits for an owner or admin to approve and paste the Tailscale auth key in Ployz Cloud.",
});

/** A running router from a daemon that predates the probe may still be one Tailscale refused, so only the approver's Connect, in Ployz Cloud, says it took. */
const onMessage = (routers: number, servers: number) => routers === servers && servers > 0
  ? `${routers} ${routers === 1 ? "router" : "routers"} running. Integrations in Ployz Cloud shows whether Tailscale took the key.`
  : `Servers are still connecting: ${routers} of ${servers} run a router. Integrations in Ployz Cloud shows each one.`;

export const privateAccessUse = defineCommand({
  name: "private_access_use",
  requires: use.requires,
  path: ["cluster", "private-access"],
  about: "Turn Private Access over Tailscale on or off. Never pass an auth key; the approver pastes it.",
  examples: ["ployz cluster private-access on", "ployz cluster private-access off"],
  kind: "write",
  removes: false,
  gate: {
    gate: "ask",
    asked: (caller, input) => Effect.succeed(privateAccessAsked(caller.organization.id, input.to)),
  },
  agents: true,
  typed: null,
  positional: ["to"],
  input: Schema.Struct({ to: To }),
  output: Schema.Struct({ to: To, routers: Schema.Number }),
  render: "message",
  run: (caller, input) => caller.support !== null
    ? Effect.succeed(refused("forbidden", "Only an owner or admin of this Organization can do this."))
    : withRuntime(runPrivateAccess(caller.organization.id, input.to)).pipe(
      Effect.map(({ servers, failures }) => {
        const routers = routerCount(servers);
        if (failures.length > 0) return refused("unavailable", failures.map(({ name, message }) => `${name}: ${message}`).join(" "));
        return { ok: true, output: { to: input.to, routers }, message: input.to === "on" ? onMessage(routers, servers.length) : "Private Access is off." } as const;
      }),
      Effect.catchTag("Validation", (problem) => Effect.succeed(refused("failed_precondition", problem.message))),
    ),
});
