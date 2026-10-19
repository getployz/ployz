// @vitest-environment jsdom
import { cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import { afterEach, expect, it, vi } from "vitest";
import { createMemoryHistory, createRootRoute, createRouter, RouterProvider } from "@tanstack/react-router";
import { Table, TableBody } from "#/components/ui/table";
import type { FleetTokenView } from "#/modules/machines/waiting/fleet-token";
import { TokenRow } from "./token-row";

afterEach(cleanup);

const token = (extra: Partial<FleetTokenView>): FleetTokenView => ({
  id: "00000000-0000-4000-8000-000000000001",
  name: "rack-1",
  expiresAt: new Date(Date.now() + 86_400_000).toISOString(),
  expired: false,
  autoEnrollMax: null,
  lastUsedAt: null,
  createdAt: new Date().toISOString(),
  createdBy: "ada@example.com",
  waiting: 0,
  joined: 0,
  ...extra,
});

async function renderRow(view: FleetTokenView, onRevoke = () => {}) {
  const root = createRootRoute({
    component: () => <Table><TableBody><TokenRow organizationSlug="ada" token={view} onRevoke={onRevoke} /></TableBody></Table>,
  });
  const router = createRouter({ routeTree: root, history: createMemoryHistory({ initialEntries: ["/"] }) });
  render(<RouterProvider router={router} />);
  return within(await screen.findByRole("row"));
}

it("shows the arrival limit and joined count beside waiting servers", async () => {
  const row = await renderRow(token({ autoEnrollMax: 3, waiting: 2, joined: 1 }));
  expect(row.getByRole("link", { name: "2 waiting" }).getAttribute("href")).toBe("/cloud/ada/~/servers?token=rack-1");
  expect(row.getByText("· 1/3 joined")).toBeTruthy();
  expect(row.getByText("Auto up to 3")).toBeTruthy();
});

it("keeps a review token's joined count without an automatic limit", async () => {
  const row = await renderRow(token({ joined: 2 }));
  expect(row.getByText("· 2 joined")).toBeTruthy();
  expect(row.queryByText(/Auto up to/)).toBeNull();
});

it("asks before revoking and names the waiting servers that won't join", async () => {
  const onRevoke = vi.fn();
  const row = await renderRow(token({ waiting: 1, joined: 0 }), onRevoke);
  fireEvent.click(row.getByRole("button", { name: "Revoke" }));

  const dialog = await screen.findByRole("alertdialog", { name: "Revoke rack-1?" });
  expect(within(dialog).getByText("1 waiting server won't join. Joined ones stay.")).toBeTruthy();
  expect(onRevoke).not.toHaveBeenCalled();

  fireEvent.click(within(dialog).getByRole("button", { name: "Revoke" }));
  expect(onRevoke).toHaveBeenCalledOnce();
});

it("keeps the token when the confirm is cancelled", async () => {
  const onRevoke = vi.fn();
  const row = await renderRow(token({}), onRevoke);
  fireEvent.click(row.getByRole("button", { name: "Revoke" }));

  const dialog = await screen.findByRole("alertdialog", { name: "Revoke rack-1?" });
  expect(within(dialog).getByText("New servers can't join with it. Joined ones stay.")).toBeTruthy();
  fireEvent.click(within(dialog).getByRole("button", { name: "Cancel" }));
  expect(onRevoke).not.toHaveBeenCalled();
});
