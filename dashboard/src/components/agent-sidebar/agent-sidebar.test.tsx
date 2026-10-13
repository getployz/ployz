// @vitest-environment jsdom
import { useState } from "react";
import { cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import type { DiffView } from "@ployz/sdk";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { asTestDouble } from "#/lib/test-double";
import { AgentButton, AgentColumn } from "./agent-sidebar";
import { pendingApprovalsOptions, type ApprovalView } from "./approvals.queries";

beforeEach(() => {
  vi.stubGlobal("fetch", vi.fn(() => new Promise(() => {})));
  vi.stubGlobal("ResizeObserver", class { observe() {} unobserve() {} disconnect() {} });
});
afterEach(() => { cleanup(); vi.unstubAllGlobals(); localStorage.clear(); });

// An Approval waiting on a human, with nothing in its plan.
const waiting = asTestDouble<ApprovalView>()({
  id: "a1",
  status: "pending",
  review: {
    effects: [],
    diff: asTestDouble<DiffView>()({ environment: { id: "e1", project: "shop", name: "production", revision: 1 }, total_count: 0, changes: [] }),
  },
});

function show(pending: ApprovalView[]) {
  const client = new QueryClient({ defaultOptions: { queries: { enabled: false, retry: false } } });
  client.setQueryData(pendingApprovalsOptions("acme").queryKey, pending);
  render(<QueryClientProvider client={client}><Agent client={client} /></QueryClientProvider>);
}

/** The top bar's Agent button and the column beside the page, sharing the Organization route's `chat` search param. */
function Agent({ client }: { client: QueryClient }) {
  const [chat, setChat] = useState<string>();
  return (
    <>
      <AgentButton organizationSlug="acme" chat={chat} onChat={setChat} />
      <AgentColumn organizationSlug="acme" collectionScope={{ queryClient: client, sessionId: "s", userId: "u" }}
        page={{ page: "architecture" }} chat={chat} onChat={setChat} />
    </>
  );
}

it("counts the approvals waiting on a human on the Agent button", () => {
  show([waiting]);
  expect(screen.getByRole("button", { name: "Agent, 1 approval waiting" }).textContent).toBe("Agent1 waiting");
  cleanup();

  show([]);
  expect(screen.getByRole("button", { name: "Agent" }).textContent).toBe("Agent");
});

it("opens into the conversation and closes from the same button", { timeout: 20_000 }, async () => {
  show([waiting]);
  const toggle = screen.getByRole("button", { name: "Agent, 1 approval waiting" });
  expect(toggle.getAttribute("aria-pressed")).toBe("false");
  fireEvent.click(toggle);
  // The first open loads the panel lazily, the chat client with it.
  const panel = await screen.findByRole("region", { name: "Ployz agent" }, { timeout: 15_000 });
  expect(within(panel).getByRole("heading", { name: "New chat" })).toBeTruthy();
  expect(within(panel).getByPlaceholderText("Ask about your servers…")).toBeTruthy();
  // The crumbs say where you are, and the Agent button is the only way out.
  expect(within(panel).queryByText(/acme/)).toBeNull();
  expect(within(panel).queryByRole("button", { name: /close/i })).toBeNull();
  // Open, the count moves into the thread, so the button stops counting.
  expect(toggle.getAttribute("aria-pressed")).toBe("true");
  expect(toggle.textContent).toBe("Agent");

  fireEvent.click(toggle);
  expect(screen.queryByRole("region", { name: "Ployz agent" })).toBeNull();
  expect(screen.getByRole("button", { name: "Agent, 1 approval waiting" }).getAttribute("aria-pressed")).toBe("false");
});

it("reopens the Organization's last thread", { timeout: 20_000 }, async () => {
  localStorage.setItem("ployz.agent.thread.acme", "thread-1");
  show([]);
  fireEvent.click(screen.getByRole("button", { name: "Agent" }));
  await screen.findByRole("region", { name: "Ployz agent" }, { timeout: 15_000 });
  fireEvent.click(screen.getByRole("button", { name: "Agent" }));
  fireEvent.click(screen.getByRole("button", { name: "Agent" }));
  expect(localStorage.getItem("ployz.agent.thread.acme")).toBe("thread-1");
});
