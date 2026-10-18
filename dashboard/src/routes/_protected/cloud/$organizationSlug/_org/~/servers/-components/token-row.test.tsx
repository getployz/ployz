// @vitest-environment jsdom
import { cleanup, render, screen, within } from "@testing-library/react";
import { afterEach, expect, it } from "vitest";
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

async function renderRow(view: FleetTokenView) {
  const root = createRootRoute({
    component: () => <Table><TableBody><TokenRow organizationSlug="ada" token={view} onRevoke={() => {}} /></TableBody></Table>,
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
