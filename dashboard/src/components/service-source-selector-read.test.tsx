// @vitest-environment jsdom
import { getDbClient } from "#/collections/scope";
import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, expect, it, vi } from "vitest";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { createMemoryHistory, createRootRoute, createRoute, createRouter, RouterProvider } from "@tanstack/react-router";
import type { ReactNode } from "react";
import { Command, CommandList } from "#/components/ui/command";
import { getGithubReposCollection, githubReposQueryKey } from "#/modules/github/github.collection";
import { githubKeys } from "#/modules/github/github.queries";
import { GitRepoSelector, ImageSelector } from "./service-source-selector";

/** The selector reads its scope from the authenticated `/_protected` route, as in the app. */
function selectorRoutes(session: { sessionId: string }, render: () => ReactNode) {
  const rootRoute = createRootRoute();
  const protectedRoute = createRoute({
    getParentRoute: () => rootRoute, id: "_protected",
    beforeLoad: () => ({ session: { user: { id: "user" }, session: { id: session.sessionId } } }),
  });
  const page = createRoute({ getParentRoute: () => protectedRoute, path: "/", component: render });
  return rootRoute.addChildren([protectedRoute.addChildren([page])]);
}

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
});

it("lets pointer users submit a valid image and blocks invalid or pending selections", () => {
  const select = vi.fn();
  const view = render(<ImageSelector onSelectImage={select} />);
  const input = screen.getByRole("textbox", { name: "Container image" });
  const submit = screen.getByRole("button", { name: "Continue" });
  expect(submit.hasAttribute("disabled")).toBe(true);
  fireEvent.change(input, { target: { value: "not an image" } });
  fireEvent.click(submit);
  expect(select).not.toHaveBeenCalled();
  fireEvent.change(input, { target: { value: " nginx:alpine " } });
  expect(submit.hasAttribute("disabled")).toBe(false);
  fireEvent.click(submit);
  expect(select).toHaveBeenCalledExactlyOnceWith("nginx:alpine");
  view.rerender(<ImageSelector onSelectImage={select} disabled />);
  expect(submit.hasAttribute("disabled")).toBe(true);
  fireEvent.click(submit);
  expect(select).toHaveBeenCalledTimes(1);
});

it.each([false, true])("shows an initial read failure and recovers (empty snapshot: %s)", async (empty) => {
  vi.stubGlobal("ResizeObserver", class { observe() {} unobserve() {} disconnect() {} });
  vi.stubGlobal("scrollTo", () => {});
  Element.prototype.scrollIntoView ??= () => {};
  const queryClient = new QueryClient({ defaultOptions: { queries: { enabled: false, retry: false, staleTime: Infinity } } });
  const scope = { queryClient, organizationSlug: "acme", sessionId: "session" };
  const githubRepos = getGithubReposCollection(scope);
  const queryKey = githubReposQueryKey(scope);
  queryClient.setQueryData(githubKeys.access("acme"), { hasInstallations: true });
  queryClient.setQueryData(githubKeys.web(), { origin: "https://github.com" });
  const failRead = () => queryClient.fetchQuery({ queryKey, queryFn: async () => { throw new Error("offline"); }, staleTime: 0 });
  await expect(failRead()).rejects.toThrow("offline");
  const routeTree = selectorRoutes(scope, () => <Command><CommandList><GitRepoSelector organizationSlug="acme" query="" onSelectRepo={() => {}} /></CommandList></Command>);
  const router = createRouter({ routeTree, history: createMemoryHistory({ initialEntries: ["/"] }) });
  try {
    render(<QueryClientProvider client={queryClient}>
      <RouterProvider router={router} />
    </QueryClientProvider>);
    expect((await screen.findByRole("alert")).textContent).toContain("Could not load");
    expect(screen.queryByText("No repositories found")).toBeNull();

    const rows = empty ? [] : [{
      id: 42, installation_id: 12, name: "repo", full_name: "acme/repo", default_branch: "main", private: true,
      html_url: "https://github.com/acme/repo", repo_updated_at: new Date().toISOString(),
      synced_at: new Date().toISOString(),
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
    await githubRepos.cleanup();
    queryClient.clear();
  }
});

it("selects the installation of the highlighted row when one repository is reachable through two", async () => {
  vi.stubGlobal("ResizeObserver", class { observe() {} unobserve() {} disconnect() {} });
  vi.stubGlobal("scrollTo", () => {});
  Element.prototype.scrollIntoView ??= () => {};
  const queryClient = new QueryClient({ defaultOptions: { queries: { enabled: false, retry: false, staleTime: Infinity } } });
  const scope = { queryClient, organizationSlug: "shared-org", sessionId: "shared-session" };
  getGithubReposCollection(scope);
  const row = (installation_id: number) => ({
    id: 42, installation_id, name: "repo", full_name: "acme/repo", default_branch: "main", private: true,
    html_url: "https://github.com/acme/repo", repo_updated_at: new Date().toISOString(), synced_at: new Date().toISOString(),
  });
  queryClient.setQueryData(githubReposQueryKey(scope), [row(12), row(13)]);
  queryClient.setQueryData(githubKeys.access("shared-org"), { hasInstallations: true });
  queryClient.setQueryData(githubKeys.web(), { origin: "https://github.com" });
  const select = vi.fn();
  const routeTree = selectorRoutes(scope, () => <Command><CommandList><GitRepoSelector organizationSlug="shared-org" query="" onSelectRepo={select} /></CommandList></Command>);
  const router = createRouter({ routeTree, history: createMemoryHistory({ initialEntries: ["/"] }) });
  try {
    const view = render(<QueryClientProvider client={queryClient}><RouterProvider router={router} /></QueryClientProvider>);
    const options = await screen.findAllByRole("option", { name: "acme/repo" });
    expect(options).toHaveLength(2);
    const root = view.container.querySelector("[cmdk-root]");
    if (!root) throw new Error("cmdk root missing");
    await waitFor(() => expect(options.map((option) => option.getAttribute("aria-selected"))).toEqual(["true", "false"]));
    fireEvent.keyDown(root, { key: "ArrowDown" });
    await waitFor(() => expect(options.map((option) => option.getAttribute("aria-selected"))).toEqual(["false", "true"]));
    fireEvent.keyDown(root, { key: "Enter" });
    expect(select).toHaveBeenCalledExactlyOnceWith(expect.objectContaining({
      access: { type: "github-installation", installationId: 13 },
    }));
  } finally {
    cleanup(); await getDbClient(queryClient).cleanup(); queryClient.clear();
  }
});

it("offers a public URL without a GitHub installation", async () => {
  vi.stubGlobal("ResizeObserver", class { observe() {} unobserve() {} disconnect() {} });
  vi.stubGlobal("scrollTo", () => {});
  Element.prototype.scrollIntoView ??= () => {};
  const queryClient = new QueryClient({ defaultOptions: { queries: { enabled: false, retry: false } } });
  const scope = { queryClient, organizationSlug: "public-org", sessionId: "public-session" };
  getGithubReposCollection(scope);
  queryClient.setQueryData(githubReposQueryKey(scope), []);
  queryClient.setQueryData(githubKeys.access("public-org"), { hasInstallations: false });
  queryClient.setQueryData(githubKeys.web(), { origin: "https://github.com" });
  const routeTree = selectorRoutes(scope, () => <Command><CommandList><GitRepoSelector organizationSlug="public-org" query="http://github.com/owner/repo.git" onSelectRepo={() => {}} /></CommandList></Command>);
  const router = createRouter({ routeTree, history: createMemoryHistory({ initialEntries: ["/"] }) });
  try {
    render(<QueryClientProvider client={queryClient}><RouterProvider router={router} /></QueryClientProvider>);
    expect(await screen.findByRole("option", { name: "Deploy owner/repo" })).toBeTruthy();
  } finally {
    cleanup(); await getDbClient(queryClient).cleanup(); queryClient.clear();
  }
});
