import type { RunErrorEvent } from "@tanstack/ai";
import { Option, Schema } from "effect";

/** Why a turn failed, in the words the sidebar shows. `chat_too_long` asks for a new chat rather than a retry. */
export const FAILURES = {
  agent_unavailable: "The agent is unavailable right now. Try again later.",
  chat_too_long: "This chat is too long for the agent. Start a new chat.",
  rate_limited: "Too many requests. Try again shortly.",
  overloaded: "Claude is overloaded. Try again shortly.",
  unreachable: "Couldn't reach Claude. Try again.",
  failed: "Claude couldn't answer. Try again.",
} as const;
export type Failure = keyof typeof FAILURES;

/** The error body Anthropic answers with, which its SDK carries on the error and prints into the message. */
const ProviderBody = Schema.Struct({ error: Schema.Struct({ type: Schema.String, message: Schema.String }) });
const ProviderJson = Schema.fromJsonString(ProviderBody);

const body = (chunk: RunErrorEvent) =>
  Schema.decodeUnknownOption(ProviderBody)(chunk.rawEvent).pipe(
    Option.orElse(() => Schema.decodeUnknownOption(ProviderJson)(chunk.message.slice(chunk.message.indexOf("{")))),
    Option.map(({ error }) => error),
  );

const byType = new Map<string, Failure>([
  ["authentication_error", "agent_unavailable"],
  ["billing_error", "agent_unavailable"],
  ["permission_error", "agent_unavailable"],
  ["not_found_error", "agent_unavailable"],
  ["request_too_large", "chat_too_long"],
  ["rate_limit_error", "rate_limited"],
  ["overloaded_error", "overloaded"],
]);

const byStatus = (status: number): Failure =>
  status === 429 ? "rate_limited"
  : status === 529 ? "overloaded"
  : status === 413 ? "chat_too_long"
  : status === 401 || status === 402 || status === 403 || status === 404 ? "agent_unavailable"
  : "failed";

/**
 * What a provider's failed turn means for the member, or nothing when the error is not the provider's: Cloud's own
 * refusals and TanStack's plain-English stops already read well. The Anthropic adapter codes every error it catches
 * with the HTTP status, and with `"undefined"` when no response came back.
 */
export const providerFailure = (chunk: RunErrorEvent): Failure | undefined => {
  const error = Option.getOrUndefined(body(chunk));
  if (error !== undefined) {
    if (error.type === "invalid_request_error") {
      if (/credit balance/i.test(error.message)) return "agent_unavailable";
      if (/prompt is too long|too many tokens/i.test(error.message)) return "chat_too_long";
      return "failed";
    }
    return byType.get(error.type) ?? "failed";
  }
  if (chunk.code === "undefined") return "unreachable";
  const status = Number(chunk.code);
  return Number.isInteger(status) && status >= 400 ? byStatus(status) : undefined;
};
