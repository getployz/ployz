import "@tanstack/react-start/server-only";
import { trace } from "@opentelemetry/api";
import { defineChatMiddleware } from "@tanstack/ai";
import { otelMiddleware } from "@tanstack/ai/middlewares/otel";
import { Effect } from "effect";
import type { PostHogService } from "#/modules/analytics/posthog.server";
import type { Caller } from "#/modules/identity/actor";

/** A span for each turn, model call and tool call on Cloud's tracer, without prompt or reply text. */
export const agentTracing = otelMiddleware({ tracer: trace.getTracer("ployz-agent") });

/**
 * A PostHog `$ai_generation` for each model call in one turn: tokens, model and latency, never prompt or reply text.
 * `promptTokens` counts cache reads and writes too, so the input sent is the uncached rest and
 * `$ai_cache_reporting_exclusive` says so, whatever the provider: PostHog prices each token once.
 */
export const generationEvents = (posthog: PostHogService, caller: Caller) => {
  let started = Date.now();
  let spanId: string | null = null;
  return defineChatMiddleware({
    name: "ployz-generation-events",
    onIteration: (_ctx, { messageId }) => {
      started = Date.now();
      spanId = messageId;
    },
    onUsage: (ctx, usage) => {
      const cacheRead = usage.promptTokensDetails?.cachedTokens ?? 0;
      const cacheWrite = usage.promptTokensDetails?.cacheWriteTokens ?? 0;
      return Effect.runPromise(posthog.capture({
        userId: caller.userId,
        organizationId: caller.organization.id,
        event: "$ai_generation",
        properties: {
          $ai_trace_id: ctx.runId,
          $ai_session_id: ctx.threadId,
          $ai_span_id: spanId,
          $ai_provider: ctx.provider,
          $ai_model: ctx.model,
          $ai_input_tokens: usage.promptTokens - cacheRead - cacheWrite,
          $ai_output_tokens: usage.completionTokens,
          $ai_cache_read_input_tokens: cacheRead,
          $ai_cache_creation_input_tokens: cacheWrite,
          $ai_cache_reporting_exclusive: true,
          $ai_latency: (Date.now() - started) / 1000,
          $ai_stream: true,
        },
      }));
    },
  });
};
