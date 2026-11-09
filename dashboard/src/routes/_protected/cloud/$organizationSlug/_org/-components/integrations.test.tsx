// @vitest-environment jsdom
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { createMemoryHistory, createRootRoute, createRouter, RouterProvider } from "@tanstack/react-router";
import { act, cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import type { ReactNode } from "react";
import { toast } from "sonner";
import { ApprovalCard } from "#/components/agent-sidebar/approval-card";
import type { ApprovalView } from "#/components/agent-sidebar/approvals.queries";
import * as approvals from "#/modules/approvals/approvals.functions";
import * as functions from "#/modules/private-access/private-access.functions";
import { machineId } from "#/test/ids";
import { Integrations } from "./integrations";

type Changed = Awaited<ReturnType<typeof functions.setPrivateAccessServerFn>>;
type Observed = NonNullable<Awaited<ReturnType<typeof functions.observePrivateAccessServerFn>>>;

const KEY = "tskey-auth-kTEST-notarealkey0123456789";
const FRA = machineId("f");
const AMS = machineId("a");
let observed: Observed;

const servers = (fra: "running" | "stopped" | "none", ams: "running" | "stopped" | "none"): Observed => ({
  servers: [{ id: FRA, name: "fra-1", router: fra }, { id: AMS, name: "ams-1", router: ams }],
  partial: false,
});

let changing: (() => Promise<Changed>) | null = null;

beforeEach(() => {
  vi.spyOn(functions, "observePrivateAccessServerFn").mockImplementation(() => Promise.resolve(observed));
  vi.spyOn(functions, "setPrivateAccessServerFn").mockImplementation(() => changing?.() ?? Promise.reject(new Error("no Connect expected")));
});

afterEach(() => {
  cleanup();
  changing = null;
  vi.restoreAllMocks();
});

async function show(page: () => ReactNode = () => <Integrations organizationSlug="acme" editable />) {
  const router = createRouter({ routeTree: createRootRoute({ component: page }), history: createMemoryHistory() });
  await router.load();
  render(<QueryClientProvider client={new QueryClient()}><RouterProvider router={router} /></QueryClientProvider>);
  await waitFor(() => expect(screen.queryByText("Checking your servers…")).toBeNull());
}

const rowOf = (name: string) => within(screen.getByRole("list", { name: "Routers" })).getByText(name).closest("li") as HTMLElement;

async function connect(changed: Changed, then: Observed, button = "Connect") {
  changing = () => {
    observed = then;
    return Promise.resolve(changed);
  };
  fireEvent.click(screen.getByRole("button", { name: button }));
  fireEvent.change(screen.getByLabelText("Auth key"), { target: { value: KEY } });
  await act(async () => { fireEvent.submit(screen.getByLabelText("Auth key").closest("form") as HTMLFormElement); });
}

it("reads partly connected while a Server has no router, and connects it without turning the others off", async () => {
  observed = servers("running", "none");
  await show();

  expect(screen.getByText("Partly connected")).toBeTruthy();
  expect(within(rowOf("ams-1")).getByText("No router")).toBeTruthy();
  expect(screen.getByRole("button", { name: "Turn off" })).toBeTruthy();

  await connect({ servers: [{ id: FRA, name: "fra-1", router: true }, { id: AMS, name: "ams-1", router: true }], failures: [] }, servers("running", "running"));

  expect(await screen.findByText("Connected")).toBeTruthy();
  expect(functions.setPrivateAccessServerFn).toHaveBeenCalledOnce();
  expect(functions.setPrivateAccessServerFn).toHaveBeenCalledWith({ data: { organizationSlug: "acme", to: "on", authKey: KEY } });
});

it("calls a Server that didn't answer Connect running once it reports its router running, not refused", async () => {
  observed = servers("none", "none");
  await show();

  const unanswered = { id: AMS, name: "ams-1", message: "ams-1 didn't answer.", problem: "unreachable" } as const;
  await connect({ servers: [{ id: FRA, name: "fra-1", router: true }], failures: [unanswered] }, servers("running", "running"));

  expect(await screen.findByText("Connected")).toBeTruthy();
  expect(within(rowOf("ams-1")).getByText("Running")).toBeTruthy();
  expect(screen.queryByText("Key refused")).toBeNull();
  expect(screen.queryByText("ams-1 didn't answer.")).toBeNull();
});

it("keeps a refused key marked on a router Docker still lists as running", async () => {
  observed = servers("none", "none");
  await show();

  const refused = (id: typeof FRA, name: string) => ({ id, name, message: `Tailscale refused the auth key on ${name}.`, problem: "key_refused" }) as const;
  await connect({ servers: [{ id: FRA, name: "fra-1", router: false }, { id: AMS, name: "ams-1", router: false }], failures: [refused(FRA, "fra-1"), refused(AMS, "ams-1")] }, servers("running", "running"));

  expect(await screen.findByText("Not connected")).toBeTruthy();
  expect(within(rowOf("fra-1")).getByText("Key refused")).toBeTruthy();
  expect(within(rowOf("ams-1")).getByText("Key refused")).toBeTruthy();
  expect(screen.getByText("Tailscale refused the auth key on ams-1.")).toBeTruthy();
});

it("marks a key the approver pasted in an Approval card refused on Integrations, and offers another key there", async () => {
  observed = servers("none", "none");
  let asked: ApprovalView = {
    id: "00000000-0000-4000-8000-0000000000a1",
    status: "pending",
    digest: "set_private_access:on",
    command: "set_private_access",
    reason: null,
    review: { effects: [], operation: { verb: "set_private_access", name: "on", preview: { to: "on" } } },
    protected: false,
    requested_by: null,
    created_at: "2026-11-09T12:00:00.000Z",
    decided_at: null,
    asker: null,
    yours: false,
  };
  vi.spyOn(approvals, "getApprovalServerFn").mockImplementation(() => Promise.resolve(asked));
  const refused = (id: typeof FRA, name: string) => ({ id, name, message: `Tailscale refused the auth key on ${name}.`, problem: "key_refused" }) as const;
  vi.spyOn(functions, "approvePrivateAccessServerFn").mockImplementation(() => {
    observed = servers("running", "running");
    asked = { ...asked, status: "approved", decided_at: "2026-11-09T12:01:00.000Z" };
    return Promise.resolve({
      approval: asked,
      sent: { servers: [{ id: FRA, name: "fra-1", router: false }, { id: AMS, name: "ams-1", router: false }], failures: [refused(FRA, "fra-1"), refused(AMS, "ams-1")] },
    });
  });
  const toasted = vi.spyOn(toast, "error");
  await show(() => (
    <>
      <ApprovalCard organizationSlug="acme" id={asked.id} seed={asked} />
      <Integrations organizationSlug="acme" editable />
    </>
  ));

  const card = screen.getByLabelText("Turn private access on needs approval");
  fireEvent.change(within(card).getByLabelText("Auth key"), { target: { value: KEY } });
  await act(async () => { fireEvent.click(within(card).getByRole("button", { name: "Connect" })); });

  expect(await screen.findByText("Approved. Integrations shows each router.")).toBeTruthy();
  expect(toasted).toHaveBeenCalledWith("Approved. Not every server connected.", {
    description: "Tailscale refused the auth key on fra-1. Tailscale refused the auth key on ams-1. Try another key or turn it off in Integrations.",
  });
  expect(await screen.findByText("Not connected")).toBeTruthy();
  expect(within(rowOf("fra-1")).getByText("Key refused")).toBeTruthy();
  expect(within(rowOf("ams-1")).getByText("Key refused")).toBeTruthy();
  expect(screen.getByRole("button", { name: "Try another key" })).toBeTruthy();
  expect(screen.getByRole("button", { name: "Turn off" })).toBeTruthy();
});
