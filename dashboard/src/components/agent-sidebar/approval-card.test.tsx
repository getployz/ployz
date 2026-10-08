// @vitest-environment jsdom
import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import type { DiffView } from "@ployz/sdk";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { asTestDouble } from "#/lib/test-double";
import { ApprovalCard } from "./approval-card";
import type { ApprovalView } from "./approvals.queries";

const fetchMock = vi.fn();
beforeEach(() => { vi.stubGlobal("fetch", fetchMock); });
afterEach(() => { cleanup(); vi.unstubAllGlobals(); fetchMock.mockReset(); });

// A Deploy that drops the pg-data Volume and also scales web.
const approval = asTestDouble<ApprovalView>()({
  id: "a1",
  status: "pending",
  digest: "sha256:plan",
  command: "admit",
  reason: null,
  created_at: "2026-10-08T12:00:00.000Z",
  decided_at: null,
  review: {
    effects: [{ kind: "deletes_volume" as const, name: "pg-data", node: "v1", path: "volumes.pg-data" }],
    diff: asTestDouble<DiffView>()({
      environment: { id: "e1", project: "shop", name: "production", revision: 4 },
      total_count: 2,
      changes: [
        { type: "volume", id: "v1", name: "pg-data", lifecycle: "delete", comparison: null, data: null, settings: [] },
        { type: "service", id: "s1", name: "web", lifecycle: "update", comparison: "head", data: null, settings: [
          { path: "web.replicas", kind: "update", before: 1, after: 2, canRestore: true, row: null },
        ] },
      ],
    }),
  },
});

function show(seed: ApprovalView, onSettled = vi.fn()) {
  const client = new QueryClient({ defaultOptions: { queries: { enabled: false, retry: false } } });
  render(<QueryClientProvider client={client}><ApprovalCard organizationSlug="acme" id={seed.id} seed={seed} onSettled={onSettled} /></QueryClientProvider>);
  return { onSettled, card: () => screen.getByLabelText("Deploy production needs approval") };
}

const answered = (status: string, reason: string | null = null) =>
  Response.json({ approval: { status, reason, decided_at: "2026-10-08T12:01:00.000Z" } });

it("leads with what the plan destroys and folds the rest to a count", () => {
  const card = show(approval).card();
  expect(within(card).getByText("Destroys 1 thing")).toBeTruthy();
  expect(within(card).getByRole("list", { name: "Destroys" }).textContent).toBe("Deletes volume pg-data. Not recoverable.");
  expect(within(card).queryByText(/1 → 2/)).toBeNull();
  fireEvent.click(within(card).getByText("1 other change"));
  expect(within(card).getByText(/web .*1 → 2/)).toBeTruthy();
});

it("approves exactly the digest shown, then settles", async () => {
  fetchMock.mockResolvedValue(answered("approved"));
  const { card, onSettled } = show(approval);
  fireEvent.keyDown(card(), { key: "Enter", metaKey: true });
  await waitFor(() => expect(onSettled).toHaveBeenCalled());
  expect(fetchMock).toHaveBeenCalledWith("/api/cli/approvals/a1", expect.objectContaining({ method: "POST", body: JSON.stringify({ approve: { digest: "sha256:plan" } }) }));
  expect(screen.getByText(/Approved/)).toBeTruthy();
});

it("denies with the reason typed, for the agent to read", async () => {
  fetchMock.mockResolvedValue(answered("denied", "keep the data"));
  const { onSettled } = show(approval);
  fireEvent.click(screen.getByRole("button", { name: "Deny with a reason…" }));
  fireEvent.change(screen.getByRole("textbox", { name: "Reason" }), { target: { value: " keep the data " } });
  fireEvent.click(screen.getByRole("button", { name: /^Deny/ }));
  await waitFor(() => expect(onSettled).toHaveBeenCalled());
  expect(fetchMock).toHaveBeenCalledWith("/api/cli/approvals/a1", expect.objectContaining({ body: JSON.stringify({ reject: { reason: "keep the data" } }) }));
  expect(screen.getByText("Denied: keep the data")).toBeTruthy();
});

it("shows Cloud's refusal and keeps the card when the answer doesn't land", async () => {
  fetchMock.mockResolvedValue(Response.json({ error: { message: "The plan changed since this was asked; run the command again to review it." } }, { status: 409 }));
  const { card, onSettled } = show(approval);
  fireEvent.keyDown(card(), { key: "Escape" });
  expect((await screen.findByRole("alert")).textContent).toBe("The plan changed since this was asked; run the command again to review it.");
  expect(onSettled).not.toHaveBeenCalled();
});

it("says the plan changed when the approval was superseded, and settles", () => {
  const { onSettled } = show({ ...approval, status: "superseded" });
  expect(screen.getByText("The plan changed since this was asked. Ask again to review the new one.")).toBeTruthy();
  expect(screen.queryByRole("button", { name: /Approve/ })).toBeNull();
  expect(onSettled).toHaveBeenCalled();
});
