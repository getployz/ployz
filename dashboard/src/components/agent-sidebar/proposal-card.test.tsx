// @vitest-environment jsdom
import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import type { MachineId } from "@ployz/sdk";
import { afterEach, expect, it, vi } from "vitest";
import * as prompt from "#/modules/approval-session/approval-prompt";
import * as functions from "#/modules/proposals/proposals.functions";
import { ProposalCard } from "./proposal-card";
import type { ProposalView } from "./proposals.queries";

const decide = vi.spyOn(functions, "decideProposalServerFn");
const target = vi.spyOn(functions, "proposalTargetServerFn");
const sign = vi.spyOn(prompt, "proposalApprovals");
afterEach(() => { cleanup(); decide.mockReset(); target.mockReset(); sign.mockReset(); });

const stop = { verb: "stop" as const, service: "web", only: null, namespace: "shop-prod-a1b2", runtime: "web" };
const m1 = "m1" as MachineId;
const restart = { verb: "restart" as const, service: "db", only: { machine: m1, name: "fra-1" }, namespace: "shop-prod-a1b2", runtime: "db" };
const start = { ...stop, verb: "start" as const };

const proposal: ProposalView = {
  id: "p1",
  status: "presented",
  project: "shop",
  environment: "production",
  environment_id: "e1",
  steps: [stop, restart, start],
  progress: [stop, restart, start].map((step, index) => ({ index: index + 1, verb: step.verb, service: step.service, state: "not_run" as const, reason: null })),
  halt: null,
  reason: null,
  requested_by: "u1",
  asker: "Ada",
  created_at: "2026-11-08T12:00:00.000Z",
  decided_at: null,
};

const copies = [{ id: "c".repeat(64), machineId: m1, hook: false }];
const machines = [{ id: m1, name: "fra-1", capabilities: [], sealing: null, deviceEnforcement: false }];
const fetched = (steps: typeof proposal.steps) => ({
  environmentId: "e1", environmentName: "production", namespace: "shop-prod-a1b2",
  steps: steps.map((step) => ({ ...step, copies })), machines, environmentMachines: [m1],
});
const signed = { bundle: { body: "{}", signatures: [] }, approvals: {} };

function show(seed: ProposalView) {
  const client = new QueryClient({ defaultOptions: { queries: { enabled: false, retry: false } } });
  render(<QueryClientProvider client={client}><ProposalCard organizationSlug="acme" id={seed.id} seed={seed} /></QueryClientProvider>);
  return screen.getByLabelText("3 steps in shop/production need approval");
}

it("lists every step in order, each on the servers' name for it, and what the plan takes away", () => {
  const card = show(proposal);
  const steps = within(card).getAllByRole("listitem").filter((item) => item.closest("ol") !== null);
  expect(steps.map((item) => item.textContent)).toEqual([
    "1.Stop webOn servers shop-prod-a1b2/web",
    "2.Restart db on fra-1On servers shop-prod-a1b2/db",
    "3.Start webOn servers shop-prod-a1b2/web",
  ]);
  expect(within(card).getByRole("list", { name: "Destroys" }).textContent).toBe("Stops service web until started againRestarts service db");
  expect(within(card).getByText(/Asked by Ada/)).toBeTruthy();
});

it("approves with the bundle this browser signed over the card's steps", async () => {
  target.mockResolvedValueOnce(fetched(proposal.steps));
  sign.mockResolvedValueOnce(signed);
  const progress = proposal.progress.map((step, index) => ({ ...step, state: index === 0 ? "accepted" as const : "waiting" as const }));
  decide.mockResolvedValueOnce({ ok: true, proposal: { ...proposal, status: "approved", progress, decided_at: "2026-11-08T12:01:00.000Z" } } as never);
  fireEvent.click(within(show(proposal)).getByRole("button", { name: "Approve all" }));
  await waitFor(() => expect(decide).toHaveBeenCalledWith({ data: { organizationSlug: "acme", id: "p1", decision: { approve: signed } } }));
  expect(sign.mock.calls[0]?.[0].steps.map((step) => step.verb)).toEqual(["stop", "restart", "start"]);
  // Every signed value is the card's: its namespace and Environment, never what Cloud's target says.
  expect(sign.mock.calls[0]?.[0].environment).toEqual({ environmentId: "e1", environmentName: "production", namespace: "shop-prod-a1b2" });
  const approved = await screen.findByLabelText("3 steps in shop/production approved");
  expect(within(approved).getAllByRole("listitem").map((item) => item.textContent)).toEqual(["1.Stop webStarting", "2.Restart db on fra-1Waiting", "3.Start webWaiting"]);
});

it("signs nothing when Cloud names another namespace, or a Container off the Server a step names", async () => {
  target.mockResolvedValueOnce({ ...fetched(proposal.steps), namespace: "shop-prod-ffff" });
  const card = show(proposal);
  fireEvent.click(within(card).getByRole("button", { name: "Approve all" }));
  expect((await within(card).findByRole("alert")).textContent).toBe("This doesn't match the proposal. Nothing was approved.");
  const elsewhere = fetched(proposal.steps);
  elsewhere.steps[1] = { ...restart, copies: [{ id: "d".repeat(64), machineId: "m2" as MachineId, hook: false }] };
  target.mockResolvedValueOnce(elsewhere);
  fireEvent.click(within(card).getByRole("button", { name: "Approve all" }));
  await waitFor(() => expect(target).toHaveBeenCalledTimes(2));
  expect((await within(card).findByRole("alert")).textContent).toBe("This doesn't match the proposal. Nothing was approved.");
  expect(sign).not.toHaveBeenCalled();
  expect(decide).not.toHaveBeenCalled();
});

it("shows how far a decided plan got, and lets the card go", () => {
  const onDismiss = vi.fn();
  const progress = proposal.progress.map((step) => step.index === 1 ? { ...step, state: "done" as const }
    : step.index === 2 ? { ...step, state: "failed" as const, reason: "fra-1 didn't accept the approval." }
    : step);
  const client = new QueryClient({ defaultOptions: { queries: { enabled: false, retry: false } } });
  const seed = { ...proposal, status: "approved" as const, progress, decided_at: "2026-11-08T12:01:00.000Z" };
  render(<QueryClientProvider client={client}><ProposalCard organizationSlug="acme" id={seed.id} seed={seed} onDismiss={onDismiss} /></QueryClientProvider>);
  const card = screen.getByLabelText("3 steps in shop/production approved");
  expect(within(card).getAllByRole("listitem").map((item) => item.textContent)).toEqual([
    "1.Stop webDone",
    "2.Restart db on fra-1Failed: fra-1 didn't accept the approval.",
    "3.Start webNot run",
  ]);
  fireEvent.click(within(card).getByRole("button", { name: "Dismiss" }));
  expect(onDismiss).toHaveBeenCalledTimes(1);
});

it("signs nothing when Cloud's steps aren't the card's", async () => {
  target.mockResolvedValueOnce(fetched([restart, stop, start]));
  const card = show(proposal);
  fireEvent.click(within(card).getByRole("button", { name: "Approve all" }));
  expect((await within(card).findByRole("alert")).textContent).toBe("This doesn't match the proposal. Nothing was approved.");
  expect(sign).not.toHaveBeenCalled();
  expect(decide).not.toHaveBeenCalled();
});

it("denies with the reason typed, and shows it once Cloud takes it", async () => {
  decide.mockResolvedValueOnce({ ok: true, proposal: { ...proposal, status: "denied", reason: "not during the sale", decided_at: "2026-11-08T12:01:00.000Z" } } as never);
  const card = show(proposal);
  fireEvent.click(within(card).getByRole("button", { name: "Deny with a reason…" }));
  fireEvent.change(within(card).getByLabelText("Reason"), { target: { value: "not during the sale" } });
  fireEvent.click(within(card).getByRole("button", { name: "Deny" }));
  await waitFor(() => expect(decide).toHaveBeenCalledWith({ data: { organizationSlug: "acme", id: "p1", decision: { reject: { reason: "not during the sale" } } } }));
  const denied = await screen.findByLabelText("3 steps in shop/production denied");
  expect(within(denied).getByText("Denied")).toBeTruthy();
  expect(within(denied).getByText("not during the sale")).toBeTruthy();
  expect(within(denied).getAllByRole("listitem").map((item) => item.textContent)).toEqual(["1.Stop web", "2.Restart db on fra-1", "3.Start web"]);
  expect(target).not.toHaveBeenCalled();
});

it("shows Cloud's refusal and keeps waiting", async () => {
  target.mockResolvedValueOnce(fetched(proposal.steps));
  sign.mockResolvedValueOnce(signed);
  decide.mockResolvedValueOnce({ ok: false, message: "It didn't run: fra-1 needs a newer Ployz to stop it. It's still waiting for approval." } as never);
  const card = show(proposal);
  fireEvent.click(within(card).getByRole("button", { name: "Approve all" }));
  expect((await within(card).findByRole("alert")).textContent).toBe("It didn't run: fra-1 needs a newer Ployz to stop it. It's still waiting for approval.");
  expect(within(card).getByRole("button", { name: "Approve all" })).toBeTruthy();
});
