// @vitest-environment jsdom
import { waitFor } from "@testing-library/react";
import { QueryClient, environmentManager } from "@tanstack/react-query";
import { createMemoryHistory, createRootRouteWithContext, createRouter } from "@tanstack/react-router";
import { afterEach, expect, it, vi } from "vitest";
import { orgStoreOptions } from "#/collections/org-store";
import { getDbClient } from "#/collections/scope";
import { organizationKeys } from "#/modules/environment-design/workspace.queries";
import { Route } from "./route";

afterEach(() => { vi.restoreAllMocks(); });

function setup(server: boolean) {
  vi.spyOn(environmentManager, "isServer").mockReturnValue(server);
  let resolve!: (value: boolean) => void;
  let reject!: (error: Error) => void;
  const promise = new Promise<boolean>((yes, no) => { resolve = yes; reject = no; });
  const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  const scope = { queryClient, sessionId: "session", userId: "user" };
  queryClient.setQueryData(organizationKeys.state("acme"), { activeOrganization: { id: "org", slug: "acme", name: "Acme" }, organizations: [] });
  const options = orgStoreOptions("acme", scope);
  // Seed an in-flight Org Store read through the real Query cache; the loader must share it.
  const request = queryClient.fetchQuery({ ...options, queryFn: () => promise });
  const context = { queryClient, session: { session: { id: "session" }, user: { id: "user" } } };
  const root = createRootRouteWithContext<typeof context>()();
  Object.assign(Route.options, { path: "/$organizationSlug", getParentRoute: () => root });
  const router = createRouter({ routeTree: root.addChildren([Route]), context,
    history: createMemoryHistory({ initialEntries: ["/acme"] }), isServer: server });
  return { resolve, reject, request, router, queryClient, options, async dispose() {
    await getDbClient(queryClient).cleanup();
    queryClient.clear();
  } };
}

it("waits for the Org Store during SSR", async () => {
  const test = setup(true);
  const ensure = vi.spyOn(test.queryClient, "ensureQueryData");
  let complete = false;
  const ready = test.router.load().then(() => { complete = true; });
  await waitFor(() => expect(ensure).toHaveBeenCalledWith(expect.objectContaining({ queryKey: test.options.queryKey })));
  expect(complete).toBe(false);
  test.resolve(true);
  await ready;
  expect(complete).toBe(true);
  await test.dispose();
});

it("commits client navigation while the Org Store loads and reuses it once warm", async () => {
  const test = setup(false);
  await test.router.load();
  expect(test.queryClient.getQueryState(test.options.queryKey)?.status).toBe("pending");
  test.resolve(true);
  await test.request;
  await test.router.load();
  expect(test.queryClient.getQueryData(test.options.queryKey)).toBe(true);
  await test.dispose();
});

it("preserves Org Store failures for the shell's content boundary", async () => {
  const test = setup(false);
  await test.router.load();
  const failure = expect(test.request).rejects.toThrow("Read failed");
  test.reject(new Error("Read failed"));
  await failure;
  expect(test.queryClient.getQueryState(test.options.queryKey)?.status).toBe("error");
  await test.dispose();
});
