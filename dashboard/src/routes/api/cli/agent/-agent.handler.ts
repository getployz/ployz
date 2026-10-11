import "@tanstack/react-start/server-only";
import type { Act, CloudQuery } from "@ployz/sdk";
import { Effect, Option, Schema } from "effect";
import { type CloudCall, callCloud } from "#/modules/agent/call-cloud.server";
import { refusal } from "#/modules/config-store/config-store.server";
import { resolveCaller } from "#/modules/identity/caller.server";
import { Validation } from "#/server/public-error";

/** An ask names its query and an Act its action; `callCloud` checks the rest and refuses what it can't answer. */
const named = <T>(envelope: Schema.Schema<unknown>) => {
  const is = Schema.is(envelope);
  return Schema.declare<T>((value): value is T => is(value));
};
const AgentAsk = Schema.Struct({ ask: named<CloudQuery>(Schema.Struct({ query: Schema.String })) });
const AgentAct = Schema.Struct({ act: named<Act>(Schema.Struct({ action: Schema.Struct({ action: Schema.String }) })) });

/** `POST /api/cli/agent/ask` and `/act`: `ployz mcp` hands an agent tool's Cloud part to Cloud, as the Caller. */
export const handleAgentRequest = Effect.fn("Agent.handleCli")(function* (request: Request, part: "ask" | "act") {
  const caller = yield* resolveCaller(request.headers);
  const body: unknown = yield* Effect.tryPromise({
    try: () => request.json(),
    catch: () => new Validation({ message: "Expected a JSON body.", userFacing: true }),
  });
  const given: Option.Option<CloudCall> = part === "ask"
    ? Option.map(Schema.decodeUnknownOption(AgentAsk)(body), ({ ask }) => ({ ask }))
    : Option.map(Schema.decodeUnknownOption(AgentAct)(body), ({ act }) => ({ act }));
  if (Option.isNone(given)) {
    return refusal({ code: "invalid_argument", message: `Expected an \`${part}\`.`, details: null });
  }
  const answer = yield* callCloud(caller, given.value, request.headers.get("x-ployz-approval"));
  return answer.ok ? Response.json(answer.value, { headers: { "cache-control": "no-store" } }) : refusal(answer.refusal);
});
