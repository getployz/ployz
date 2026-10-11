// @vitest-environment jsdom
import { useState } from "react";
import { cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { asTestDouble } from "#/lib/test-double";
import { AgentSidebar } from "./agent-sidebar";
import { pendingApprovalsOptions, type ApprovalView } from "./approvals.queries";

beforeEach(() => {
  vi.stubGlobal("fetch", vi.fn(() => new Promise(() => {})));
  vi.stubGlobal("ResizeObserver", class { observe() {} unobserve() {} disconnect() {} });
});
afterEach(() => { cleanup(); vi.unstubAllGlobals(); localStorage.clear(); });

function show(pending: ApprovalView[]) {
  const client = new QueryClient({ defaultOptions: { queries: { enabled: false, retry: false } } });
  client.setQueryData(pendingApprovalsOptions("acme").queryKey, pending);
  render(<QueryClientProvider client={client}><Sidebar client={client} /></QueryClientProvider>);
}

/** The Organization route's `chat` search param, held in state. */
function Sidebar({ client }: { client: QueryClient }) {
  const [chat, setChat] = useState<string>();
  return (
    <AgentSidebar canvas scope={{ kind: "environment", organizationSlug: "acme", projectSlug: "shop", environmentSlug: "production" }}
      collectionScope={{ queryClient: client, sessionId: "s", userId: "u" }} page={{ page: "architecture" }} chat={chat} onChat={setChat} />
  );
}

it("counts the approvals waiting on a human on the collapsed tab", () => {
  show([asTestDouble<ApprovalView>()({ id: "a1", status: "pending" })]);
  const tab = screen.getByRole("button", { name: "Open agent, 1 approval waiting" });
  expect(tab.textContent).toBe("1");
  cleanup();

  show([]);
  expect(screen.getByRole("button", { name: "Open agent" }).textContent).toBe("");
});

it("opens into the conversation for the Organization and Environment, and closes back to the tab", { timeout: 20_000 }, async () => {
  show([]);
  fireEvent.click(screen.getByRole("button", { name: "Open agent" }));
  // The first open loads the panel lazily, the chat client with it.
  const panel = await screen.findByRole("region", { name: "Ployz agent" }, { timeout: 15_000 });
  expect(within(panel).getByText("acme / production")).toBeTruthy();
  expect(within(panel).getByPlaceholderText("Ask about your servers…")).toBeTruthy();
  fireEvent.click(within(panel).getByRole("button", { name: "Close agent" }));
  expect(screen.queryByRole("region", { name: "Ployz agent" })).toBeNull();
  expect(screen.getByRole("button", { name: "Open agent" })).toBeTruthy();
});
