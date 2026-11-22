import "@tanstack/react-start/server-only";
import { Effect, Schema } from "effect";
import { defineCommand, refused } from "#/modules/commands/define";
import type { CloudStore } from "#/modules/config-store/store-sdk.server";
import { wantWords } from "#/modules/ingress/doors";
import { requestDoorsSwitch, switchAsked } from "#/modules/ingress/doors.server";
import { OPERATIONS } from "#/modules/operations/operations";
import type { OrganizationRuntime } from "#/modules/runtime/organization-runtime.server";

const use = OPERATIONS["ingress.use"];

/** The command's ask looks at the Servers and reads the Store, so its run names the same services; a command's two halves share one requirement. */
const withRuntime = <A, E, R>(effect: Effect.Effect<A, E, R>): Effect.Effect<A, E, R | OrganizationRuntime | CloudStore> => effect;

const Where = Schema.Literals(["public", "cloudflare"]).annotate({
  description: "Where web traffic goes: public or cloudflare. Cloudflare Tunnel must already be set up in Ployz Cloud.",
});

export const webTrafficUse = defineCommand({
  name: "web_traffic_use",
  requires: use.requires,
  path: ["cluster", "web-traffic"],
  about: "Switch web traffic between public ports and Cloudflare. Every generated address moves with it.",
  title: "Switch web traffic",
  examples: ["ployz cluster web-traffic cloudflare", "ployz cluster web-traffic public"],
  kind: "write",
  removes: false,
  // A second call joins the switch already running.
  idempotent: true,
  gate: {
    gate: "ask",
    // A Store Cloud can't open is Cloud's own failure, not a refusal the caller can act on.
    asked: (caller, input) => switchAsked(caller.organization.id, input.to === "cloudflare" ? "tunnel" : "public").pipe(
      Effect.catchTag("ConfigStoreOpenFailure", (failure) => Effect.die(failure)),
    ),
  },
  agents: true,
  openWorld: false,
  typed: null,
  positional: ["to"],
  input: Schema.Struct({ to: Where }),
  output: Schema.Struct({ to: Where, state: Schema.String }),
  render: "message",
  run: (caller, input) => caller.support !== null
    ? Effect.succeed(refused("forbidden", "Only an owner or admin of this Organization can do this."))
    : withRuntime(requestDoorsSwitch({ organizationId: caller.organization.id, userId: caller.userId, to: input.to === "cloudflare" ? "tunnel" : "public" })).pipe(
      Effect.map(({ change, joined }) => ({
        ok: true,
        output: { to: input.to, state: change.state },
        message: `${joined ? "Already switching" : "Switching"} to ${wantWords(change.want)}. Follow it in Ployz Cloud, under Organization, Network.`,
      }) as const),
      Effect.catchTag("Conflict", (conflict) => Effect.succeed(refused("conflict", conflict.message))),
    ),
});
