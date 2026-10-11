import "@tanstack/react-start/server-only";
import { chatParamsFromRequest, toServerSentEventsResponse } from "@tanstack/ai";
import { reconstructChat } from "@tanstack/ai-persistence";
import { Effect } from "effect";
import { agentChat } from "#/modules/agent/agent-chat.server";
import { agentPersistence, threadAvailable } from "#/modules/agent/persistence.server";
import type { Caller } from "#/modules/identity/actor";
import { memberCaller } from "#/modules/identity/caller.server";
import { Database } from "#/server/database.server";
import { Forbidden, Validation } from "#/server/public-error";

const scopeOf = (caller: Caller) => ({ organizationId: caller.organization.id, userId: caller.userId });

/** `POST /api/agent/<slug>/chat`: one sidebar turn, or its resumption after an approval, streamed as server-sent events. */
export const handleAgentChat = Effect.fn("Agent.handleChat")(function* (request: Request, organizationSlug: string) {
  const caller = yield* memberCaller(request.headers, organizationSlug);
  const scope = scopeOf(caller);
  const params = yield* Effect.tryPromise({
    try: () => chatParamsFromRequest(request),
    catch: () => new Validation({ message: "Expected a chat request.", userFacing: true }),
  });
  if (!(yield* threadAvailable(scope, params.threadId))) return yield* new Forbidden({ message: "That thread is another member's." });
  const abortController = new AbortController();
  request.signal.addEventListener("abort", () => abortController.abort(), { once: true });
  const stream = yield* agentChat(caller, { ...params, abortController });
  return toServerSentEventsResponse(stream, { abortController });
});

/** `GET /api/agent/<slug>/chat?threadId=`: the member's thread, its running turn and pending approvals, for a reload. */
export const handleAgentThread = Effect.fn("Agent.handleThread")(function* (request: Request, organizationSlug: string) {
  const scope = scopeOf(yield* memberCaller(request.headers, organizationSlug));
  const persistence = yield* agentPersistence(scope);
  const run = Effect.runPromiseWith(yield* Effect.context<Database>());
  return yield* Effect.promise(() => reconstructChat(persistence, request, { authorize: (threadId) => run(threadAvailable(scope, threadId)) }));
});
