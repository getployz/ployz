// @vitest-environment jsdom
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { createMemoryHistory, createRootRoute, createRouter, RouterProvider } from "@tanstack/react-router";
import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import * as fleetTokenFunctions from "#/modules/machines/waiting/fleet-token.functions";
import * as waitingFunctions from "#/modules/machines/waiting/waiting.functions";
import { STALL_MS } from "#/hooks/use-exclusive-mutation";
import { AddServersDialog } from "./add-servers-dialog";

const TOKEN = "pflt_test-fixture-token";

beforeEach(() => {
  vi.spyOn(fleetTokenFunctions, "listFleetTokensServerFn").mockResolvedValue([]);
  vi.spyOn(waitingFunctions, "listWaitingServersServerFn").mockResolvedValue([]);
});

afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
});

async function renderDialog() {
  const router = createRouter({
    routeTree: createRootRoute({ component: () => <AddServersDialog organizationSlug="ada" /> }),
    history: createMemoryHistory({ initialEntries: ["/"] }),
  });
  await router.load();
  render(<QueryClientProvider client={new QueryClient()}><RouterProvider router={router} /></QueryClientProvider>);
  await screen.findByRole("button", { name: "Add servers" });
}

async function mintWith(expiresIn?: string) {
  const create = vi.spyOn(fleetTokenFunctions, "createFleetTokenServerFn").mockResolvedValue({
    token: TOKEN,
    id: "token-1",
    name: "brisk-otter",
    expiresAt: "2026-10-24T10:00:00Z",
    cloudOrigin: "https://ployz.dev",
    ployzVersion: "1.2.3",
  });
  await renderDialog();
  fireEvent.click(screen.getByRole("button", { name: "Add servers" }));
  if (expiresIn !== undefined) fireEvent.click(screen.getByRole("button", { name: expiresIn }));
  fireEvent.click(screen.getByRole("button", { name: "Create" }));
  return create;
}

const occurrences = (text: string) => text.split(TOKEN).length - 1;

it("puts the token in each tab's line exactly once", async () => {
  await mintWith();
  await screen.findByRole("tab", { name: "Shell" });

  for (const tab of ["Shell", "cloud-init", "Terraform"]) {
    await act(async () => fireEvent.click(screen.getByRole("tab", { name: tab })));
    const panel = screen.getByRole("tabpanel");
    expect(occurrences(panel.querySelector("code")?.textContent ?? ""), tab).toBe(1);
  }
});

it("says the line is shown once and when it expires", async () => {
  const create = await mintWith("7 days");

  expect(await screen.findByText("Shown once. Expires in 7d.")).toBeTruthy();
  expect(create).toHaveBeenCalledWith({
    data: { organizationSlug: "ada", name: null, expiresIn: "7d", autoEnroll: null },
  });
});

it("reads a one-day token's lifetime in hours", async () => {
  await mintWith();

  expect(await screen.findByText("Shown once. Expires in 24h.")).toBeTruthy();
});

it("shows a Shell line that installs the Cloud's own release", async () => {
  mintWith();
  expect(await screen.findByText(`curl -fsSL https://ployz.sh | sudo PLOYZ_FLEET_TOKEN=${TOKEN} sh -s -- 1.2.3`)).toBeTruthy();
});

it("describes the token it submitted, not the form as it reads after", async () => {
  let resolve: (minted: Awaited<ReturnType<typeof fleetTokenFunctions.createFleetTokenServerFn>>) => void = () => {};
  vi.spyOn(fleetTokenFunctions, "createFleetTokenServerFn").mockReturnValue(new Promise((settle) => { resolve = settle; }));
  await renderDialog();
  fireEvent.click(screen.getByRole("button", { name: "Add servers" }));
  fireEvent.click(screen.getByRole("button", { name: "Create" }));
  fireEvent.click(screen.getByRole("button", { name: "7 days" }));
  await act(async () => resolve({
    token: TOKEN,
    id: "token-1",
    name: "brisk-otter",
    expiresAt: "2026-10-18T10:00:00Z",
    cloudOrigin: "https://ployz.dev",
    ployzVersion: "1.2.3",
  }));

  expect(await screen.findByText("Shown once. Expires in 24h.")).toBeTruthy();
});

it("submits a bounded arrival token and describes it after minting", async () => {
  const create = vi.spyOn(fleetTokenFunctions, "createFleetTokenServerFn").mockResolvedValue({
    token: TOKEN, id: "token-auto", name: "auto", expiresAt: "2026-11-17T10:00:00Z", cloudOrigin: "https://ployz.dev", ployzVersion: "1.2.3",
  });
  await renderDialog();
  fireEvent.click(screen.getByRole("button", { name: "Add servers" }));
  expect(screen.queryByRole("spinbutton", { name: "Maximum servers" })).toBeNull();
  fireEvent.click(screen.getByRole("switch", { name: "Enroll on arrival" }));
  const max = screen.getByRole("spinbutton", { name: "Maximum servers" });
  for (const value of ["", "0", "1001", "1.5"]) {
    fireEvent.change(max, { target: { value } });
    expect(screen.getByRole("button", { name: "Create" }).hasAttribute("disabled")).toBe(true);
  }
  fireEvent.change(max, { target: { value: "3" } });
  fireEvent.click(screen.getByRole("button", { name: "Create" }));
  expect(await screen.findByText("Auto up to 3")).toBeTruthy();
  expect(screen.getByText("Installs Docker and Ployz. Up to 3 servers join your existing cluster when ready.")).toBeTruthy();
  expect(create).toHaveBeenCalledWith({ data: { organizationSlug: "ada", name: null, expiresIn: "1d", autoEnroll: { maxServers: 3 } } });
});

it("shows the one-time command when Create is double-clicked", async () => {
  const create = vi.spyOn(fleetTokenFunctions, "createFleetTokenServerFn")
    .mockResolvedValueOnce({ token: TOKEN, id: "token-1", name: "soak-add-dbl", expiresAt: "2026-10-19T10:00:00Z", cloudOrigin: "https://ployz.dev", ployzVersion: "1.2.3" })
    .mockRejectedValueOnce(new Error("A Fleet Token named soak-add-dbl already exists."));
  await renderDialog();
  fireEvent.click(screen.getByRole("button", { name: "Add servers" }));
  fireEvent.change(screen.getByPlaceholderText("Picked for you"), { target: { value: "soak-add-dbl" } });
  const submit = screen.getByRole("button", { name: "Create" });
  fireEvent.click(submit);
  fireEvent.click(submit);

  expect(await screen.findByText("Shown once. Expires in 24h.")).toBeTruthy();
  expect(create).toHaveBeenCalledTimes(1);
});

it("holds the dialog through Escape until the command shows", async () => {
  let resolve: (minted: Awaited<ReturnType<typeof fleetTokenFunctions.createFleetTokenServerFn>>) => void = () => {};
  vi.spyOn(fleetTokenFunctions, "createFleetTokenServerFn").mockReturnValue(new Promise((settle) => { resolve = settle; }));
  await renderDialog();
  fireEvent.click(screen.getByRole("button", { name: "Add servers" }));
  const submit = screen.getByRole("button", { name: "Create" });
  fireEvent.click(submit);
  await waitFor(() => expect(submit.hasAttribute("disabled")).toBe(true));
  fireEvent.keyDown(document.activeElement ?? document.body, { key: "Escape" });
  await act(async () => resolve({ token: TOKEN, id: "token-1", name: "brisk-otter", expiresAt: "2026-10-19T10:00:00Z", cloudOrigin: "https://ployz.dev", ployzVersion: "1.2.3" }));

  expect(await screen.findByText("Shown once. Expires in 24h.")).toBeTruthy();
});

it("lets go of a mint that never answers, so Escape closes the dialog", async () => {
  vi.useFakeTimers({ shouldAdvanceTime: true });
  try {
    vi.spyOn(fleetTokenFunctions, "createFleetTokenServerFn").mockReturnValue(new Promise(() => {}));
    await renderDialog();
    fireEvent.click(screen.getByRole("button", { name: "Add servers" }));
    const submit = screen.getByRole("button", { name: "Create" });
    fireEvent.click(submit);
    await waitFor(() => expect(submit.hasAttribute("disabled")).toBe(true));
    await act(async () => vi.advanceTimersByTime(STALL_MS));

    expect(screen.getByText("Ployz hasn't answered. Retry or cancel.")).toBeTruthy();
    expect(screen.getByRole("button", { name: "Retry" }).hasAttribute("disabled")).toBe(false);
    fireEvent.keyDown(document.activeElement ?? document.body, { key: "Escape" });
    await waitFor(() => expect(screen.queryByText("New fleet token")).toBeNull());
  } finally {
    vi.useRealTimers();
  }
});
