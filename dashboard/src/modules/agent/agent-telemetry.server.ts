import "@tanstack/react-start/server-only";
import { trace } from "@opentelemetry/api";
import { defineChatMiddleware } from "@tanstack/ai";
import { otelMiddleware } from "@tanstack/ai/middlewares/otel";
import { Effect, Schema } from "effect";
import type { PostHogService } from "#/modules/analytics/posthog.server";
import type { Caller } from "#/modules/identity/actor";

type Properties = Record<string, string | number | boolean | null>;

// A refused or cancelled call still finishes without throwing, so its outcome says it failed.
const refused = Schema.is(Schema.Struct({ ok: Schema.Literal(false) }));

/** A span for each turn, model call and tool call on Cloud's tracer, without prompt or reply text. */
export const agentTracing = otelMiddleware({ tracer: trace.getTracer("ployz-agent") });

/**
 * PostHog AI Observability for one turn, never prompt, reply, tool arguments, results or errors: an `$ai_trace` per run, under
 * it an `$ai_generation` per model call naming the tools it called, and under that an `$ai_span` per tool call.
 * `promptTokens` counts cache reads and writes too, so the input sent is the uncached rest and
 * `$ai_cache_reporting_exclusive` says so, whatever the provider: PostHog prices each token once.
 */
export const aiObservability = (posthog: PostHogService, caller: Caller) => {
  let started = Date.now();
  // The model call the tools run under; none yet when a resumed run first finishes the calls it was waiting on.
  let spanId: string | null = null;
  let toolsCalled: string[] = [];
  const capture = (event: string, properties: Properties) =>
    Effect.runPromise(posthog.capture({ userId: caller.userId, organizationId: caller.organization.id, event, properties }));
  return defineChatMiddleware({
    name: "ployz-ai-observability",
    onIteration: (_ctx, { messageId }) => {
      started = Date.now();
      spanId = messageId;
      toolsCalled = [];
    },
    onChunk: (_ctx, chunk) => {
      if (chunk.type === "TOOL_CALL_START") toolsCalled.push(chunk.toolCallName);
    },
    onUsage: (ctx, usage) => {
      const cacheRead = usage.promptTokensDetails?.cachedTokens ?? 0;
      const cacheWrite = usage.promptTokensDetails?.cacheWriteTokens ?? 0;
      const generation = {
        $ai_trace_id: ctx.runId,
        $ai_session_id: ctx.threadId,
        $ai_span_id: spanId,
        $ai_parent_id: ctx.runId,
        $ai_provider: ctx.provider,
        $ai_model: ctx.model,
        $ai_input_tokens: usage.promptTokens - cacheRead - cacheWrite,
        $ai_output_tokens: usage.completionTokens,
        $ai_cache_read_input_tokens: cacheRead,
        $ai_cache_creation_input_tokens: cacheWrite,
        $ai_cache_reporting_exclusive: true,
        $ai_latency: (Date.now() - started) / 1000,
        $ai_stream: true,
      };
      return capture("$ai_generation", toolsCalled.length === 0
        ? generation
        : { ...generation, $ai_tools_called: toolsCalled.join(","), $ai_tool_call_count: toolsCalled.length });
    },
    onAfterToolCall: (ctx, { toolCallId, toolName, ok, result, duration }) =>
      capture("$ai_span", {
        $ai_trace_id: ctx.runId,
        $ai_session_id: ctx.threadId,
        $ai_span_id: toolCallId,
        $ai_parent_id: spanId ?? ctx.runId,
        $ai_span_name: toolName,
        $ai_latency: duration / 1000,
        $ai_is_error: !ok || refused(result),
      }),
    onFinish: (ctx, { duration }) =>
      capture("$ai_trace", { $ai_trace_id: ctx.runId, $ai_session_id: ctx.threadId, $ai_span_name: "sidebar_turn", $ai_latency: duration / 1000 }),
    onError: (ctx, { duration }) =>
      capture("$ai_trace", {
        $ai_trace_id: ctx.runId,
        $ai_session_id: ctx.threadId,
        $ai_span_name: "sidebar_turn",
        $ai_latency: duration / 1000,
        $ai_is_error: true,
      }),
  });
};
