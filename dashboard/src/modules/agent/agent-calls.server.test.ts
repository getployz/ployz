import { PostHogMCP } from "@posthog/mcp";
import { Effect } from "effect";
import { afterEach, beforeEach, expect, it } from "vitest";
import { asTestDouble } from "#/lib/test-double";
import { INTENT_REFUSAL, runAgentCall } from "#/modules/agent/agent-runner.server";
import type { AgentCaller } from "#/modules/identity/actor";
import { type AgentCallReport, type AgentSurface, McpAnalytics, type McpAnalyticsService, recordAgentCall, sessionIdOf } from "#/modules/agent/agent-calls.server";

type Sent = { readonly event: string; readonly distinctId: string | undefined; readonly properties: Readonly<Record<string, string | number | boolean | null | ReadonlyArray<string>>> };

let sent: Sent[] = [];
let client: PostHogMCP;

// The real MCP Analytics client, with its wire events caught before they leave: what PostHog would receive.
beforeEach(() => {
  sent = [];
  client = new PostHogMCP("phc_test", {
    host: "http://127.0.0.1:9",
    enableExceptionAutocapture: false,
    before_send: (event) => {
      if (event !== null) sent.push({ event: event.event, distinctId: event.distinctId,properties: event.properties ?? {} });
      return null;
    },
  });
});
afterEach(async () => {
  await client.shutdown();
});

const analytics: McpAnalyticsService = { record: (send) => Effect.sync(() => send(client)) };
const withPostHog = <A>(effect: Effect.Effect<A>) => Effect.runPromise(effect.pipe(Effect.provideService(McpAnalytics, analytics)));

const callReport = (surface: AgentSurface, rest: { tool?: string; intent?: string | null } = {}): AgentCallReport => ({
  kind: "call",
  surface,
  tool: rest.tool ?? "service_list",
  intent: rest.intent === undefined ? "Check what is running." : rest.intent,
  error: null,
  session: sessionIdOf("a conversation"),
  startedAt: new Date("2026-11-17T10:00:00.000Z"),
  durationMs: 40,
  client: { name: "claude-code", version: "2.1.0" },
  serverVersion: "0.42.0",
  userAgent: null,
  protocolVersion: null,
  caller: { userId: "user-1", organizationId: "org-1" },
});

const flushed = async () => {
  await client.flush();
  return sent;
};

it.each<[AgentSurface, string]>([
  ["cloud", "ployz-cloud"],
  ["sidebar", "ployz-sidebar"],
  ["local", "ployz-local"],
  ["cli", "ployz-cli"],
])("a call from %s is one $mcp_tool_call named %s, with its reason and its session", async (surface, server) => {
  await withPostHog(recordAgentCall(callReport(surface)));
  const events = await flushed();
  expect(events.map(({ event }) => event)).toEqual(["$mcp_tool_call"]);
  expect(events[0]).toMatchObject({
    distinctId: "user-1",
    properties: {
      $mcp_source: "posthog_mcp_analytics",
      $mcp_server_name: server,
      $mcp_tool_name: "service_list",
      $mcp_intent: "Check what is running.",
      $mcp_intent_source: "context_parameter",
      $mcp_is_error: false,
      $session_id: sessionIdOf("a conversation"),
    },
  });
});

it("an email and a token in the reason never reach PostHog, and neither do tool arguments or results", async () => {
  await withPostHog(recordAgentCall(callReport("cloud", { intent: "Email ada@example.com the key sk-ant-api03-abcdefghijklmnopqrstuvwxyz0123456789 for the audit." })));
  const [event] = await flushed();
  const wire = JSON.stringify(event);
  expect(wire).not.toContain("ada@example.com");
  expect(wire).not.toContain("abcdefghijklmnopqrstuvwxyz0123456789");
  expect(Object.keys(event?.properties ?? {})).not.toEqual(expect.arrayContaining(["$mcp_parameters"]));
  expect(Object.keys(event?.properties ?? {})).not.toEqual(expect.arrayContaining(["$mcp_response"]));
});

it.each([
  "wJalrXUtnFEMI/K7MDENG/bPxRfiCYzEXAMPLEKEY",
  "wJalrXUtnFEMI/KxMDENG/bPxRfiCYzEXAMPLEKEY",
  "q3N8vT+2xLm/9KfPz0aRbE7wYc1HdUj4sGiOk6lMnV8=",
  "MIIEvQIBADANBgkqhkiG9w0BAQEFAASCBKcwggSjAgEAAoIBAQC7+aB/x",
])("the secret %s in a reason is redacted whole, and a path or URL beside it is kept", async (secret) => {
  const kept = "/home/user/Projects/acme/version2/src/index.ts, see https://example.com/docs/getting-started/install.";
  await withPostHog(recordAgentCall(callReport("cli", { intent: `Rotate ${secret} for ${kept}` })));
  const [event] = await flushed();
  expect(event?.properties["$mcp_intent"]).toBe(`Rotate [redacted] for ${kept}`);
});

it("a call refused for lacking a reason is a failed call of type validation, with no intent", async () => {
  const caller = asTestDouble<AgentCaller>()({ userId: "user-1", organization: { id: "org-1", slug: "shop" } });
  const origin = { surface: "cloud", session: sessionIdOf("x"), client: { name: null, version: null }, serverVersion: null, userAgent: null, protocolVersion: null } as const;
  const answer = await withPostHog(runAgentCall(caller, { tool: "environment_show", input: { intent: "  " }, key: "k", approvalId: null, page: { at: "organization" }, origin }) as Effect.Effect<unknown>);
  expect(answer).toEqual({ outcome: { ok: false, refusal: { code: "invalid_argument", message: INTENT_REFUSAL, details: null } } });
  const events = await flushed();
  expect(events).toHaveLength(1);
  expect(events[0]?.properties).toMatchObject({ $mcp_tool_name: "environment_show", $mcp_is_error: true, $mcp_error_type: "validation" });
  expect(events[0]?.properties).not.toHaveProperty("$mcp_intent");
});

it("a tool the catalog does not have is an $mcp_unknown_tool, never a $mcp_tool_call", async () => {
  const caller = asTestDouble<AgentCaller>()({ userId: "user-1", organization: { id: "org-1", slug: "shop" } });
  const origin = { surface: "cloud", session: sessionIdOf("x"), client: { name: null, version: null }, serverVersion: null, userAgent: null, protocolVersion: null } as const;
  await withPostHog(runAgentCall(caller, { tool: "my password is hunter2", input: { intent: "Try it." }, key: "k", approvalId: null, page: { at: "organization" }, origin }) as Effect.Effect<unknown>);
  const events = await flushed();
  expect(events.map(({ event }) => event)).toEqual(["$mcp_unknown_tool"]);
  expect(JSON.stringify(events)).not.toContain("hunter2");
});
