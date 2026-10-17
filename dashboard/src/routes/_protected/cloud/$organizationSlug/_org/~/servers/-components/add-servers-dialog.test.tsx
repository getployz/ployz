// @vitest-environment jsdom
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import * as fleetTokenFunctions from "#/modules/machines/waiting/fleet-token.functions";
import * as waitingFunctions from "#/modules/machines/waiting/waiting.functions";
import { AddServersDialog } from "./add-servers-dialog";

const TOKEN = "pflt_test-fixture-token";

beforeEach(() => {
  vi.spyOn(waitingFunctions, "listWaitingServersServerFn").mockResolvedValue([]);
});

afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
});

function mintWith(expiresIn?: string) {
  const create = vi.spyOn(fleetTokenFunctions, "createFleetTokenServerFn").mockResolvedValue({
    token: TOKEN,
    id: "token-1",
    name: "brisk-otter",
    expiresAt: "2026-10-24T10:00:00Z",
    cloudOrigin: "https://ployz.dev",
  });
  render(
    <QueryClientProvider client={new QueryClient()}>
      <AddServersDialog organizationSlug="ada" />
    </QueryClientProvider>,
  );
  fireEvent.click(screen.getByRole("button", { name: "Add servers" }));
  if (expiresIn !== undefined) fireEvent.click(screen.getByRole("button", { name: expiresIn }));
  fireEvent.click(screen.getByRole("button", { name: "Create" }));
  return create;
}

const occurrences = (text: string) => text.split(TOKEN).length - 1;

it("puts the token in each tab's line exactly once", async () => {
  mintWith();
  await screen.findByRole("tab", { name: "Shell" });

  for (const tab of ["Shell", "cloud-init", "Terraform"]) {
    await act(async () => fireEvent.click(screen.getByRole("tab", { name: tab })));
    const panel = screen.getByRole("tabpanel");
    expect(occurrences(panel.querySelector("code")?.textContent ?? ""), tab).toBe(1);
  }
});

it("says the line is shown once and when it expires", async () => {
  const create = mintWith("7 days");

  expect(await screen.findByText("Shown once. Expires in 7d.")).toBeTruthy();
  expect(create).toHaveBeenCalledWith({
    data: { organizationSlug: "ada", name: null, expiresIn: "7d", autoEnroll: null },
  });
});

it("reads a one-day token's lifetime in hours", async () => {
  mintWith();

  expect(await screen.findByText("Shown once. Expires in 24h.")).toBeTruthy();
});
