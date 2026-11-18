// @vitest-environment jsdom
import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import type { MachineId } from "@ployz/sdk";
import { afterEach, expect, it, vi } from "vitest";
import * as functions from "#/modules/proposals/proposals.functions";
import { ProposalCard } from "./proposal-card";
import type { ProposalView } from "./proposals.queries";

const decide = vi.spyOn(functions, "decideProposalServerFn");
afterEach(() => { cleanup(); decide.mockReset(); });

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

it("approves every step, then shows how far the plan got", async () => {
  const progress = proposal.progress.map((step, index) => ({ ...step, state: index === 0 ? "accepted" as const : "waiting" as const }));
  decide.mockResolvedValueOnce({ ok: true, proposal: { ...proposal, status: "approved", progress, decided_at: "2026-11-08T12:01:00.000Z" } } as never);
  fireEvent.click(within(show(proposal)).getByRole("button", { name: "Approve all" }));
  await waitFor(() => expect(decide).toHaveBeenCalledWith({ data: { organizationSlug: "acme", id: "p1", decision: { approve: {} } } }));
  const approved = await screen.findByLabelText("3 steps in shop/production approved");
  expect(within(approved).getAllByRole("listitem").map((item) => item.textContent)).toEqual(["1.Stop webStarting", "2.Restart db on fra-1Waiting", "3.Start webWaiting"]);
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

it("shows a renamed Service by the name the servers run, in the step and in what the plan takes away", () => {
  const renamed = { ...proposal, steps: [{ ...stop, runtime: "web-old" }, restart, start] };
  const card = show(renamed);
  expect(within(card).getAllByRole("listitem").filter((item) => item.closest("ol") !== null)[0]?.textContent).toBe("1.Stop web-old (web)On servers shop-prod-a1b2/web-old");
  expect(within(card).getByRole("list", { name: "Destroys" }).textContent).toBe("Stops service web-old (web) until started againRestarts service db");
});

it("dates a decided card by when it was asked, not when it was decided", () => {
  vi.useFakeTimers({ now: new Date("2026-11-08T12:10:00.000Z") });
  try {
    const client = new QueryClient({ defaultOptions: { queries: { enabled: false, retry: false } } });
    const seed = { ...proposal, status: "approved" as const, decided_at: "2026-11-08T12:09:50.000Z" };
    render(<QueryClientProvider client={client}><ProposalCard organizationSlug="acme" id={seed.id} seed={seed} /></QueryClientProvider>);
    const card = screen.getByLabelText("3 steps in shop/production approved");
    expect(within(card).getByText(/Asked by Ada/).textContent).toBe("Asked by Ada · 10 minutes ago");
  } finally {
    vi.useRealTimers();
  }
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
});

it("shows Cloud's refusal and keeps waiting", async () => {
  decide.mockResolvedValueOnce({ ok: false, message: "It didn't run: fra-1 needs a newer Ployz to stop it. It's still waiting for approval." } as never);
  const card = show(proposal);
  fireEvent.click(within(card).getByRole("button", { name: "Approve all" }));
  expect((await within(card).findByRole("alert")).textContent).toBe("It didn't run: fra-1 needs a newer Ployz to stop it. It's still waiting for approval.");
  expect(within(card).getByRole("button", { name: "Approve all" })).toBeTruthy();
});
