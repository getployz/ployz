// @vitest-environment jsdom
import { cleanup, render, screen } from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import type { UIMessage } from "@tanstack/ai-react";
import { afterEach, expect, it } from "vitest";
import { asTestDouble } from "#/lib/test-double";
import { Message } from "./agent-panel";
import { approvalOptions, type ApprovalView } from "./approvals.queries";

afterEach(() => { cleanup(); });

const scope = { queryClient: new QueryClient(), sessionId: "s", userId: "u" };

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
  render(<QueryClientProvider client={client}><Message organizationSlug="acme" scope={scope} message={message} asked={{ c1: "a1" }} bound={[]} /></QueryClientProvider>);
  expect(screen.getAllByText(/denied/i).map((line) => line.textContent)).toEqual(["Denied: keep the data"]);
});
