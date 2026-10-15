// @vitest-environment jsdom
import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import type { DiffView } from "@ployz/sdk";
import { afterEach, expect, it, vi } from "vitest";
import { asTestDouble } from "#/lib/test-double";
import * as functions from "#/modules/approvals/approvals.functions";
import { ApprovalCard } from "./approval-card";
import type { ApprovalView } from "./approvals.queries";

const decide = vi.spyOn(functions, "decideApprovalServerFn");
afterEach(() => { cleanup(); decide.mockReset(); });

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

const answered = (status: ApprovalView["status"], reason: string | null = null) =>
  ({ ok: true as const, approval: { ...approval, status, reason, decided_at: "2026-10-08T12:01:00.000Z" } }) as never;

it("leads with what the plan destroys and folds the rest to a count", () => {
  const card = show(approval).card();
  expect(within(card).getByText("Deletes 1 volume")).toBeTruthy();
  expect(within(card).getByRole("list", { name: "Destroys" }).textContent).toBe("Deletes volume pg-data. Not recoverable.");
  expect(within(card).queryByText(/1 → 2/)).toBeNull();
  fireEvent.click(within(card).getByText("1 other change"));
  expect(within(card).getByText(/web .*1 → 2/)).toBeTruthy();
});

it("approves exactly the digest shown, then settles", async () => {
  decide.mockResolvedValue(answered("approved"));
  const { card, onSettled } = show(approval);
  fireEvent.keyDown(card(), { key: "Enter", metaKey: true });
  await waitFor(() => expect(onSettled).toHaveBeenCalled());
  expect(decide).toHaveBeenCalledWith({ data: { organizationSlug: "acme", id: "a1", decision: { approve: { digest: "sha256:plan" } } } });
  expect(screen.getByText(/Approved/)).toBeTruthy();
});

it("denies with the reason typed, for the agent to read", async () => {
  decide.mockResolvedValue(answered("denied", "keep the data"));
  const { onSettled } = show(approval);
  fireEvent.click(screen.getByRole("button", { name: "Deny with a reason…" }));
  fireEvent.change(screen.getByRole("textbox", { name: "Reason" }), { target: { value: " keep the data " } });
  fireEvent.click(screen.getByRole("button", { name: /^Deny/ }));
  await waitFor(() => expect(onSettled).toHaveBeenCalled());
  expect(decide).toHaveBeenCalledWith({ data: { organizationSlug: "acme", id: "a1", decision: { reject: { reason: "keep the data" } } } });
  expect(screen.getByText("Denied: keep the data")).toBeTruthy();
});

it("shows Cloud's refusal and keeps the card when the answer doesn't land", async () => {
  decide.mockResolvedValue({ ok: false, message: "The plan changed since this was asked; run the command again to review it." } as never);
  const { card, onSettled } = show(approval);
  fireEvent.keyDown(card(), { key: "Escape" });
  expect((await screen.findByRole("alert")).textContent).toBe("The plan changed since this was asked; run the command again to review it.");
  expect(onSettled).not.toHaveBeenCalled();
});

it("names a Server operation by its verb and Server, with nothing folded", () => {
  const removal = asTestDouble<ApprovalView>()({
    ...approval,
    command: "remove",
    review: {
      effects: [{ kind: "removes_server" as const, name: "fra-1", node: "m1", path: "servers/m1" }],
      operation: { verb: "remove" as const, name: "fra-1", preview: { machine: "m1" } },
    },
  });
  show(removal);
  const card = within(screen.getByLabelText("Remove fra-1 needs approval"));
  expect(card.getByText("Remove fra-1")).toBeTruthy();
  expect(card.getByRole("list", { name: "Destroys" }).textContent).toBe("Removes server fra-1");
  expect(card.queryByText(/other change/)).toBeNull();
});

it("says a Save deletes nothing until the next deploy, and approving it saves without deploying", async () => {
  decide.mockResolvedValue(({ ok: true, approval: { ...approval, command: "publish", status: "approved", decided_at: "2026-10-08T12:01:00.000Z" } }) as never);
  const { onSettled } = show(asTestDouble<ApprovalView>()({ ...approval, command: "publish" }));
  const card = within(screen.getByLabelText("Save production needs approval"));
  expect(card.getByText("Save production")).toBeTruthy();
  expect(card.getByText("Deletes on deploy")).toBeTruthy();
  expect(card.getByRole("list", { name: "Destroys" }).textContent).toBe("Next deploy deletes volume pg-data.");
  fireEvent.click(card.getByRole("button", { name: /^Approve/ }));
  await waitFor(() => expect(onSettled).toHaveBeenCalled());
  expect(screen.getByText("Approved. Saved, not deployed.")).toBeTruthy();
});

it("counts a Deploy's deleted Volumes, and anything else it destroys as things", () => {
  const two = { ...approval.review, effects: [approval.review.effects[0], { kind: "deletes_volume" as const, name: "cache", node: "v2", path: "volumes.cache" }] };
  show(asTestDouble<ApprovalView>()({ ...approval, review: two }));
  expect(within(screen.getByLabelText("Deploy production needs approval")).getByText("Deletes 2 volumes")).toBeTruthy();
  cleanup();
  const mixed = { ...approval.review, effects: [approval.review.effects[0], { kind: "removes_service" as const, name: "web", node: "s1", path: "services.s1" }] };
  show(asTestDouble<ApprovalView>()({ ...approval, review: mixed }));
  expect(within(screen.getByLabelText("Deploy production needs approval")).getByText("Destroys 2 things")).toBeTruthy();
});

it("words a destroyed thing without a name as its kind, never by its id", () => {
  const review = { ...approval.review, effects: [{ kind: "removes_service" as const, node: "0b9c5d1e-svc", path: "services.0b9c5d1e-svc" }] };
  show(asTestDouble<ApprovalView>()({ ...approval, review }));
  expect(within(screen.getByLabelText("Deploy production needs approval")).getByRole("list", { name: "Destroys" }).textContent).toBe("Removes a service");
});

it("says the plan changed when the approval was superseded, and settles", () => {
  const { onSettled } = show({ ...approval, status: "superseded" });
  expect(screen.getByText("The plan changed since this was asked. Ask again to review the new one.")).toBeTruthy();
  expect(screen.queryByRole("button", { name: /Approve/ })).toBeNull();
  expect(onSettled).toHaveBeenCalled();
});
