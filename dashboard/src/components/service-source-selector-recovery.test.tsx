// @vitest-environment jsdom
import { act, cleanup, render, screen, waitFor } from "@testing-library/react";
import { afterEach, expect, it, vi } from "vitest";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { createMemoryHistory, createRootRoute, createRoute, createRouter, RouterProvider } from "@tanstack/react-router";
import { Command, CommandList } from "#/components/ui/command";
import { getGithubReposCollection, githubReposQueryKey } from "#/modules/github/github.collection";
import { githubKeys } from "#/modules/github/github.queries";
import { GitRepoSelector } from "./service-source-selector";

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
});

it("an open picker recovers a failed first load when the next read succeeds", async () => {
  vi.stubGlobal("ResizeObserver", class { observe() {} unobserve() {} disconnect() {} });
  vi.stubGlobal("scrollTo", () => {});
  Element.prototype.scrollIntoView ??= () => {};
  const queryClient = new QueryClient({ defaultOptions: { queries: { enabled: false, retry: false, staleTime: Infinity } } });
  const scope = { queryClient, organizationSlug: "acme", sessionId: "session" };
  const githubRepos = getGithubReposCollection(scope);
  const queryKey = githubReposQueryKey(scope);
  queryClient.setQueryData(githubKeys.access("acme"), { hasInstallations: true });
  queryClient.setQueryData(githubKeys.web(), { origin: "https://github.com" });
  const rootRoute = createRootRoute();
  const protectedRoute = createRoute({
    getParentRoute: () => rootRoute, id: "_protected",
    beforeLoad: () => ({ session: { user: { id: "user" }, session: { id: "session" } } }),
  });
  const page = createRoute({ getParentRoute: () => protectedRoute, path: "/",
    component: () => <Command><CommandList><GitRepoSelector organizationSlug="acme" query="" onSelectRepo={() => {}} /></CommandList></Command> });
  const router = createRouter({ routeTree: rootRoute.addChildren([protectedRoute.addChildren([page])]), history: createMemoryHistory({ initialEntries: ["/"] }) });
  const read = (rows: () => unknown[]) => queryClient.fetchQuery({ queryKey, queryFn: async () => rows(), staleTime: 0 });
  try {
    render(<QueryClientProvider client={queryClient}><RouterProvider router={router} /></QueryClientProvider>);
    await waitFor(() => expect(githubRepos.subscriberCount).toBeGreaterThan(0));
    await act(async () => { await expect(read(() => { throw new Error("offline"); })).rejects.toThrow("offline"); });
    expect((await screen.findByRole("alert")).textContent).toContain("Could not load");
    expect(githubRepos.status).toBe("error");
    const now = new Date().toISOString();
    const rows = () => [{ id: 42, installation_id: 12, name: "repo", full_name: "acme/repo", default_branch: "main", private: true,
      html_url: "https://github.com/acme/repo", repo_updated_at: now, synced_at: now }];
    await act(async () => { await read(rows); });
    // Queries are disabled here, so this read stands in for the one the recovery reload's observer starts.
    await waitFor(() => expect(githubRepos.health.reloading).toBe(true));
    await act(async () => { await read(rows); });
    await screen.findByRole("option", { name: "acme/repo" });
    expect(githubRepos.status).toBe("ready");
    expect(screen.queryByRole("alert")).toBeNull();
  } finally {
    cleanup();
    await githubRepos.cleanup();
    queryClient.clear();
  }
});
