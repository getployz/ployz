import "@tanstack/react-start/server-only";
import { agentCloudCall } from "@ployz/sdk";
import { Effect, Result } from "effect";
import { type CloudCall, callCloud } from "#/modules/agent/call-cloud.server";
import { refusal } from "#/modules/config-store/config-store.server";
import { resolveCaller } from "#/modules/identity/caller.server";
import { Validation } from "#/server/public-error";

/** `POST /api/cli/agent/ask` and `/act`: `ployz mcp` hands an agent tool's Cloud part to Cloud, as the Caller. */
export const handleAgentRequest = Effect.fn("Agent.handleCli")(function* (request: Request, part: "ask" | "act") {
  const caller = { ...yield* resolveCaller(request.headers), via: "agent" } as const;
  const body: unknown = yield* Effect.tryPromise({
    try: () => request.json(),
    catch: () => new Validation({ message: "Expected a JSON body.", userFacing: true }),
  });
  const given = yield* Effect.result(Effect.try({ try: (): CloudCall => agentCloudCall(body), catch: (error) => (error instanceof Error ? error.message : String(error)) }));
  if (Result.isFailure(given) || !(part in given.success)) {
    const message = Result.isFailure(given) ? given.failure : `Expected an \`${part}\`.`;
    return refusal({ code: "invalid_argument", message, details: null });
  }
  const answer = yield* callCloud(caller, given.success, request.headers.get("x-ployz-approval"));
  return answer.ok ? Response.json(answer.value, { headers: { "cache-control": "no-store" } }) : refusal(answer.refusal);
});
