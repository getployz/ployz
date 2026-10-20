// @vitest-environment jsdom
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { createMemoryHistory, createRootRoute, createRouter, RouterProvider } from "@tanstack/react-router";
import { act, cleanup, render, screen } from "@testing-library/react";
import { afterEach, expect, it, vi } from "vitest";
import * as connectFunctions from "#/modules/github/github-connect.functions";
import { GithubSettings } from "./github-settings";

type Listing = Awaited<ReturnType<typeof connectFunctions.listOrganizationGithubServerFn>>;

afterEach(() => {
  cleanup();
  vi.useRealTimers();
  vi.restoreAllMocks();
});

const hopper = (repositories: number, repositoriesSynced: boolean): Listing => ({
  canManage: true,
  accounts: [{ login: "hopper", avatarUrl: null, repositories, repositoriesSynced, connectedBy: "Ada", connectedAt: "2026-10-19T00:00:00Z" }],
});

it("says repositories appear in a moment until the first sync lands, then counts them", async () => {
  vi.useFakeTimers({ shouldAdvanceTime: true });
  const list = vi.spyOn(connectFunctions, "listOrganizationGithubServerFn")
    .mockResolvedValueOnce(hopper(0, false))
    .mockResolvedValue(hopper(150, true));
  const router = createRouter({
    routeTree: createRootRoute({
      component: () => <GithubSettings organizationSlug="shop" notice={undefined} offer={undefined} clearConnectSearch={() => {}} />,
    }),
    history: createMemoryHistory(),
  });
  await router.load();
  render(<QueryClientProvider client={new QueryClient()}><RouterProvider router={router} /></QueryClientProvider>);

  expect(await screen.findByText(/Repositories appear in a moment/)).toBeTruthy();
  await act(() => vi.advanceTimersByTimeAsync(2_500));
  expect(await screen.findByText(/150 repositories/)).toBeTruthy();
  expect(screen.queryByText(/Repositories appear in a moment/)).toBeNull();

  await act(() => vi.advanceTimersByTimeAsync(10_000));
  expect(list).toHaveBeenCalledTimes(2);
});

it("counts zero repositories once a sync has found none", async () => {
  vi.spyOn(connectFunctions, "listOrganizationGithubServerFn").mockResolvedValue(hopper(0, true));
  const router = createRouter({
    routeTree: createRootRoute({
      component: () => <GithubSettings organizationSlug="shop" notice={undefined} offer={undefined} clearConnectSearch={() => {}} />,
    }),
    history: createMemoryHistory(),
  });
  await router.load();
  render(<QueryClientProvider client={new QueryClient()}><RouterProvider router={router} /></QueryClientProvider>);

  expect(await screen.findByText(/0 repositories/)).toBeTruthy();
});

it("links an existing installation beside Connect GitHub", async () => {
  vi.spyOn(connectFunctions, "listOrganizationGithubServerFn").mockResolvedValue({ canManage: true, accounts: [] });
  const router = createRouter({
    routeTree: createRootRoute({
      component: () => <GithubSettings organizationSlug="shop" notice={undefined} offer={undefined} clearConnectSearch={() => {}} />,
    }),
    history: createMemoryHistory(),
  });
  await router.load();
  render(<QueryClientProvider client={new QueryClient()}><RouterProvider router={router} /></QueryClientProvider>);

  const existing = await screen.findAllByRole("button", { name: "Use existing" });
  expect(existing.map((link) => link.getAttribute("href"))).toContain("/api/github/connect?organization=shop&existing=1");
});
