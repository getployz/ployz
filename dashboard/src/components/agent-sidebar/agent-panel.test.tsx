// @vitest-environment jsdom
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import type { UIMessage } from "@tanstack/ai-react";
import { afterEach, expect, it, vi } from "vitest";
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
