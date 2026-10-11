// @vitest-environment jsdom
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import type { UIMessage } from "@tanstack/ai-react";
import { afterEach, expect, it, vi } from "vitest";
import type { JsonObject } from "#/db/tables";
import { asTestDouble } from "#/lib/test-double";
import AgentPanel, { Turn } from "./agent-panel";
import { approvalOptions, type ApprovalView } from "./approvals.queries";

afterEach(() => { cleanup(); localStorage.clear(); vi.unstubAllGlobals(); });

const scope = { queryClient: new QueryClient(), sessionId: "s", userId: "u" };
const page = { page: "architecture", project: "web", environment: "production", service: "api" };

it("says a denied call was denied once, on its approval card", () => {
  const client = new QueryClient({ defaultOptions: { queries: { enabled: false, retry: false } } });
  client.setQueryData(approvalOptions("acme", "a1").queryKey, asTestDouble<ApprovalView>()({
    id: "a1", status: "denied", digest: "sha256:plan", command: "admit", reason: "keep the data",
    created_at: "2026-10-08T12:00:00.000Z", decided_at: "2026-10-08T12:01:00.000Z", review: { effects: [] },
  }));
  const message = asTestDouble<UIMessage>()({
    id: "m1",
    role: "assistant",
    parts: [{
      type: "tool-call", id: "c1", name: "deploy", arguments: "{}", state: "input-complete",
      output: { ok: false, refusal: { code: "approval_denied", message: "A human denied this deploy: keep the data", details: { approval: { reason: "keep the data" } } } },
    }],
  });
  render(<QueryClientProvider client={client}><Turn organizationSlug="acme" scope={scope} message={message} asked={{ c1: "a1" }} bound={[]} /></QueryClientProvider>);
  expect(screen.getAllByText(/denied/i).map((line) => line.textContent)).toEqual(["Denied: keep the data"]);
});

it("switching Organization with the panel open talks to the new Organization's thread", async () => {
  vi.stubGlobal("ResizeObserver", class { observe() {} unobserve() {} disconnect() {} });
  const fetched = vi.fn((_url: string) => Promise.resolve(Response.json({ messages: [] })));
  vi.stubGlobal("fetch", fetched);
  const client = new QueryClient({ defaultOptions: { queries: { enabled: false, retry: false } } });
  localStorage.setItem("ployz.agent.thread.acme", "thread-acme");
  localStorage.setItem("ployz.agent.thread.other", "thread-other");
  const panel = (organizationSlug: string) => (
    <QueryClientProvider client={client}>
      <AgentPanel organizationSlug={organizationSlug} environment={null} scope={scope} page={page} threadId={`thread-${organizationSlug}`}
        onNewChat={() => {}} onClose={() => {}} />
    </QueryClientProvider>
  );
  const shown = render(panel("acme"));
  await waitFor(() => expect(fetched.mock.lastCall?.[0]).toMatch(/^\/api\/agent\/acme\/chat\?.*threadId=thread-acme/));
  shown.rerender(panel("other"));
  await waitFor(() => expect(fetched.mock.lastCall?.[0]).toMatch(/^\/api\/agent\/other\/chat\?.*threadId=thread-other/));
});

it("sends on Enter, keeps Shift+Enter and composing input as a draft", async () => {
  vi.stubGlobal("ResizeObserver", class { observe() {} unobserve() {} disconnect() {} });
  const fetched = vi.fn((_url: string, _init?: RequestInit) => Promise.resolve(Response.json({ messages: [] })));
  vi.stubGlobal("fetch", fetched);
  const client = new QueryClient({ defaultOptions: { queries: { enabled: false, retry: false } } });
  render(<QueryClientProvider client={client}><AgentPanel organizationSlug="acme" environment={null} scope={scope} page={page} threadId="thread-acme"
    onNewChat={() => {}} onClose={() => {}} /></QueryClientProvider>);
  const posted = () => fetched.mock.calls.filter(([, init]) => init?.method === "POST").map(([, init]) => String(init?.body));
  const box = screen.getByRole("textbox", { name: "Message the agent" });
  fireEvent.change(box, { target: { value: "list services" } });
  fireEvent.keyDown(box, { key: "Enter", shiftKey: true });
  fireEvent.keyDown(box, { key: "Enter", isComposing: true });
  expect(box).toHaveProperty("value", "list services");
  fireEvent.keyDown(box, { key: "Enter" });
  await waitFor(() => expect(posted().some((body) => body.includes("list services"))).toBe(true));
  expect(box).toHaveProperty("value", "");
  expect(posted().map((body) => (JSON.parse(body) as { forwardedProps?: unknown }).forwardedProps)).toContainEqual(expect.objectContaining({ page }));
});

it("shows a member message without the page block it opens with", () => {
  const message = asTestDouble<UIMessage>()({
    id: "m1",
    role: "user",
    parts: [
      { type: "text", content: '<dashboard-page page="architecture" project="web"/>' },
      { type: "text", content: "restart this service" },
    ],
  });
  render(<QueryClientProvider client={new QueryClient()}><Turn organizationSlug="acme" scope={scope} message={message} asked={{}} bound={[]} /></QueryClientProvider>);
  expect(screen.getByText("restart this service").textContent).toBe("restart this service");
  expect(screen.queryByText(/dashboard-page/)).toBeNull();
});

it("stops a streaming reply from the composer, and the request it stops is cut", async () => {
  vi.stubGlobal("ResizeObserver", class { observe() {} unobserve() {} disconnect() {} });
  const signals: AbortSignal[] = [];
  const fetched = vi.fn((_url: string, init?: RequestInit) => {
    if (init?.method !== "POST") return Promise.resolve(Response.json({ messages: [] }));
    if (init.signal) signals.push(init.signal);
    const started = `data: ${JSON.stringify({ type: "RUN_STARTED", threadId: "thread-acme", runId: "r1", timestamp: 0 })}\n\n`;
    const body = new ReadableStream({ start: (controller) => controller.enqueue(new TextEncoder().encode(started)) });
    return Promise.resolve(new Response(body, { headers: { "content-type": "text/event-stream" } }));
  });
  vi.stubGlobal("fetch", fetched);
  const client = new QueryClient({ defaultOptions: { queries: { enabled: false, retry: false } } });
  render(<QueryClientProvider client={client}><AgentPanel organizationSlug="acme" environment={null} scope={scope} page={page} threadId="thread-acme"
    onNewChat={() => {}} onClose={() => {}} /></QueryClientProvider>);
  const box = screen.getByRole("textbox", { name: "Message the agent" });
  fireEvent.change(box, { target: { value: "explain" } });
  fireEvent.keyDown(box, { key: "Enter" });

  fireEvent.click(await screen.findByRole("button", { name: "Stop" }));
  expect(signals.map((signal) => signal.aborted)).toEqual([true]);
  await waitFor(() => expect(screen.queryByRole("button", { name: "Stop" })).toBeNull());
  expect(screen.getByRole("button", { name: "Send" })).toBeDefined();
});

const events = (...chunks: ReadonlyArray<JsonObject>) =>
  new Response(chunks.map((chunk) => `data: ${JSON.stringify({ threadId: "thread-acme", timestamp: 0, ...chunk })}\n\n`).join(""),
    { headers: { "content-type": "text/event-stream" } });

it("retries a failed reply once the wait the model's provider asked for has passed", async () => {
  vi.stubGlobal("ResizeObserver", class { observe() {} unobserve() {} disconnect() {} });
  const replies = [
    () => events({ type: "RUN_STARTED", runId: "r1" }, { type: "RUN_ERROR", runId: "r1", message: "Rate limited", metadata: { tanstack: { retryAfterMs: 1500 } } }),
    () => events({ type: "RUN_STARTED", runId: "r2" }, { type: "TEXT_MESSAGE_START", messageId: "a1", role: "assistant" },
      { type: "TEXT_MESSAGE_CONTENT", messageId: "a1", delta: "Back now." }, { type: "TEXT_MESSAGE_END", messageId: "a1" },
      { type: "RUN_FINISHED", runId: "r2", finishReason: "stop" }),
  ];
  const posts: string[] = [];
  vi.stubGlobal("fetch", vi.fn((_url: string, init?: RequestInit) => {
    if (init?.method !== "POST") return Promise.resolve(Response.json({ messages: [] }));
    posts.push(String(init.body));
    return Promise.resolve(replies[posts.length - 1]?.() ?? Response.json({}, { status: 500 }));
  }));
  const client = new QueryClient({ defaultOptions: { queries: { enabled: false, retry: false } } });
  render(<QueryClientProvider client={client}><AgentPanel organizationSlug="acme" environment={null} scope={scope} page={page} threadId="thread-acme"
    onNewChat={() => {}} onClose={() => {}} /></QueryClientProvider>);
  const box = screen.getByRole("textbox", { name: "Message the agent" });
  fireEvent.change(box, { target: { value: "list services" } });
  fireEvent.keyDown(box, { key: "Enter" });

  expect((await screen.findByRole("alert")).textContent).toBe("Rate limited");
  expect(screen.getByRole("button", { name: "Retry in 2s" })).toHaveProperty("disabled", true);
  fireEvent.click(await screen.findByRole("button", { name: "Retry" }, { timeout: 3000 }));
  expect(await screen.findByText("Back now.")).toBeDefined();
  expect(posts).toHaveLength(2);
  expect(posts[1]).toContain("list services");
  expect(screen.queryByRole("alert")).toBeNull();
});
