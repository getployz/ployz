// @vitest-environment jsdom
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { createMemoryHistory, createRootRoute, createRouter, RouterProvider } from "@tanstack/react-router";
import { act, cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
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

async function show() {
  const router = createRouter({ routeTree: createRootRoute({ component: () => <Integrations organizationSlug="acme" editable /> }), history: createMemoryHistory() });
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
