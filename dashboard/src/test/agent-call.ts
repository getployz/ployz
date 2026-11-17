import { type FakeResponse, type FakeResponseStep, fakeText } from "@tanstack/ai/testing";
import { asRecord } from "#/lib/json";
import { type AgentCall, type AgentCaller, runAgentCall } from "#/modules/agent/agent-runner.server";

/** A call as hosted /mcp would hand it over, for tests that care about what the tool does and not who called. */
const FROM_MCP: AgentCall["origin"] = {
  surface: "cloud",
  session: `ses_${"0".repeat(32)}`,
  client: { name: null, version: null },
  serverVersion: null,
  userAgent: null,
  protocolVersion: null,
};

const TEST_INTENT = "Test the tool.";

/** `runAgentCall` for a call that says why it is made, as every tool requires. */
export const runAsAgent = (caller: AgentCaller, call: Omit<AgentCall, "origin">) =>
  runAgentCall(caller, { ...call, input: { ...asRecord(call.input), intent: TEST_INTENT }, origin: FROM_MCP });

type Scripted = ReturnType<typeof fakeText>;

const saying = (answer: FakeResponse): FakeResponse =>
  answer.toolCalls === undefined
    ? answer
    : { ...answer, toolCalls: answer.toolCalls.map((call) => ({ ...call, input: { ...asRecord(call.input), intent: TEST_INTENT } })) };

const stepSaying = (step: FakeResponseStep): FakeResponseStep => (step instanceof Function ? async (call) => saying(await step(call)) : saying(step));

/** A scripted model whose tool calls each say why they are made, as the sidebar's tools require of a real one. */
export const scriptedAgent = (options?: Parameters<typeof fakeText>[0]): Scripted => {
  const model = fakeText(options);
  const set = model.setResponses.bind(model);
  const append = model.appendResponses.bind(model);
  model.setResponses = (steps) => set(steps.map(stepSaying));
  model.appendResponses = (steps) => append(steps.map(stepSaying));
  return model;
};
