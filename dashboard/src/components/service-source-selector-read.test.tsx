// @vitest-environment jsdom
import { act, cleanup, render, screen, waitFor } from "@testing-library/react";
import { afterEach, expect, it, vi } from "vitest";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { createMemoryHistory, createRootRoute, createRouter, RouterProvider } from "@tanstack/react-router";
import { Command, CommandList } from "#/components/ui/command";
import { getRawGithubReposCollection, githubReposQueryKey } from "#/modules/github/github.collection";
import { githubKeys } from "#/modules/github/github.queries";
import { GitRepoSelector } from "./service-source-selector";

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
});

it.each([false, true])("shows an initial read failure and recovers (empty snapshot: %s)", async (empty) => {
  vi.stubGlobal("ResizeObserver", class { observe() {} unobserve() {} disconnect() {} });
  vi.stubGlobal("scrollTo", () => {});
  Element.prototype.scrollIntoView ??= () => {};
  const queryClient = new QueryClient({ defaultOptions: { queries: { enabled: false, retry: false, staleTime: Infinity } } });
  const scope = { queryClient, userId: "user", sessionId: "session" };
  const raw = getRawGithubReposCollection(scope);
  const queryKey = githubReposQueryKey(scope);
  queryClient.setQueryData(githubKeys.access(), { configured: true, hasInstallations: true });
  queryClient.setQueryData(githubKeys.installUrl(), { url: null });
  const failRead = () => queryClient.fetchQuery({ queryKey, queryFn: async () => { throw new Error("offline"); }, staleTime: 0 });
  await expect(failRead()).rejects.toThrow("offline");
  const rootRoute = createRootRoute({
    loader: () => ({ session: { user: { id: "user" }, session: { id: "session" } } }),
    component: () => <Command><CommandList><GitRepoSelector query="" onSelectRepo={() => {}} /></CommandList></Command>,
  });
  const router = createRouter({ routeTree: rootRoute, history: createMemoryHistory({ initialEntries: ["/"] }) });
  try {
    render(<QueryClientProvider client={queryClient}>
      <RouterProvider router={router} />
    </QueryClientProvider>);
    expect((await screen.findByRole("alert")).textContent).toContain("Could not load");
    expect(screen.queryByText("No repositories found")).toBeNull();

    const rows = empty ? [] : [{
      userId: "user", installationId: 12, repositoryId: 42, name: "repo", fullName: "acme/repo",
      defaultBranch: "main", private: true, htmlUrl: "https://github.com/acme/repo",
      repoUpdatedAt: new Date(), syncedAt: new Date(),
    }];
    await act(async () => { await queryClient.fetchQuery({ queryKey, queryFn: async () => rows, staleTime: 0 }); });
    if (empty) {
      await screen.findByText("No repositories found");
      await waitFor(() => expect(screen.queryByRole("alert")).toBeNull());
      return;
    }
    await screen.findByRole("option", { name: "acme/repo" });
    await waitFor(() => expect(screen.queryByRole("alert")).toBeNull());
    await act(async () => { await expect(failRead()).rejects.toThrow("offline"); });
    expect((await screen.findByRole("alert")).textContent).toContain("may be out of date");
    expect(screen.getByRole("option", { name: "acme/repo" })).toBeTruthy();
    await act(async () => { await queryClient.fetchQuery({ queryKey, queryFn: async () => [], staleTime: 0 }); });
    await screen.findByText("No repositories found");
    await waitFor(() => expect(screen.queryByRole("alert")).toBeNull());
  } finally {
    cleanup();
    await raw.cleanup();
    queryClient.clear();
  }
});
