import { chat, EventType, type RunErrorEvent } from "@tanstack/ai";
import { createAnthropicChat } from "@tanstack/ai-anthropic";
import { expect, it } from "vitest";
import { providerFailure } from "#/modules/agent/provider-error";

/** The real Anthropic adapter, with Anthropic answering every request `status` with `error`. */
const failingClaude = (status: number, error: { type: string; message: string }) =>
  createAnthropicChat("claude-sonnet-5-5", "sk-test", {
    maxRetries: 0,
    fetch: () => Promise.resolve(Response.json({ type: "error", error, request_id: "req_1" }, { status })),
  });

/** The RUN_ERROR the real Anthropic adapter yields when Anthropic answers `status` with `error`. */
const failedTurn = async (status: number, error: { type: string; message: string }) => {
  const stream = chat({ adapter: failingClaude(status, error), messages: [{ role: "user", content: "hi" }] });
  for await (const chunk of stream) if (chunk.type === EventType.RUN_ERROR) return chunk;
  throw new Error("The turn did not fail.");
};

it("tells the member the agent is unavailable when Cloud's Anthropic credit runs out, without the provider's JSON", async () => {
  const chunk = await failedTurn(400, {
    type: "invalid_request_error",
    message: "Your credit balance is too low to access the Anthropic API. Please go to Plans & Billing to upgrade or purchase credits.",
  });
  expect(chunk.message).toContain("{");
  expect(providerFailure(chunk)).toBe("agent_unavailable");
});

it.each([
  [401, "authentication_error", "invalid x-api-key", "agent_unavailable"],
  [429, "rate_limit_error", "Number of requests has exceeded your rate limit", "rate_limited"],
  [529, "overloaded_error", "Overloaded", "overloaded"],
  [400, "invalid_request_error", "prompt is too long: 210000 tokens > 200000 maximum", "chat_too_long"],
  [400, "invalid_request_error", "messages.1.content: field required", "failed"],
  [500, "api_error", "Internal server error", "failed"],
])("reads Anthropic's %i %s as %s", async (status, type, message, failure) => {
  expect(providerFailure(await failedTurn(status, { type, message }))).toBe(failure);
});

const runError = (chunk: Partial<RunErrorEvent>): RunErrorEvent => ({ type: EventType.RUN_ERROR, timestamp: 0, message: "", ...chunk });

it("reads an overload Anthropic sends mid-stream, which carries no status", () => {
  const body = { type: "error", error: { type: "overloaded_error", message: "Overloaded" } };
  expect(providerFailure(runError({ message: JSON.stringify(body), code: "undefined", rawEvent: body }))).toBe("overloaded");
});

it("reads a request that never reached Anthropic as unreachable", () => {
  expect(providerFailure(runError({ message: "Connection error.", code: "undefined" }))).toBe("unreachable");
});

it("leaves Cloud's own refusals and TanStack's plain stops as they are", () => {
  expect(providerFailure(runError({ message: "This message is already being answered.", code: "run_exists" }))).toBeUndefined();
  expect(providerFailure(runError({ message: "The response was cut off because the maximum token limit was reached.", code: "max_tokens" })))
    .toBeUndefined();
  expect(providerFailure(runError({ message: "Request aborted", code: "aborted" }))).toBeUndefined();
});
