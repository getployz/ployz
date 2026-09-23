// @vitest-environment jsdom
import { waitFor } from "@testing-library/react";
import { QueryClient, environmentManager } from "@tanstack/react-query";
import { createMemoryHistory, createRootRouteWithContext, createRouter } from "@tanstack/react-router";
import { afterEach, expect, it, vi } from "vitest";
import { getDbClient } from "#/collections/scope";
import { projectPreviewsOptions } from "#/modules/environment-design/workspace-queries";
import { Route } from "./index";

afterEach(() => { vi.restoreAllMocks(); });

function setup(server: boolean) {
  vi.spyOn(environmentManager, "isServer").mockReturnValue(server);
  let resolve!: (value: boolean) => void;
  let reject!: (error: Error) => void;
  const promise = new Promise<boolean>((yes, no) => { resolve = yes; reject = no; });
  const preview = { promise, resolve, reject };
  const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  const scope = { queryClient, sessionId: "session", userId: "user" };
  queryClient.setQueryData(["collections", "session", "user", "acme", "project"], [{ id: "project", organizationId: "org", slug: "store", name: "Store" }]);
  for (const table of ["environment_summary", "project_preference"]) {
    queryClient.setQueryData(["collections", "session", "user", "acme", table], []);
  }
  const options = projectPreviewsOptions("acme", scope);
  const request = queryClient.fetchQuery({ ...options, queryFn: () => preview.promise });
  const context = { queryClient, session: { session: { id: "session" }, user: { id: "user" } } };
  const root = createRootRouteWithContext<typeof context>()();
  Object.assign(Route.options, { path: "/$organizationSlug", getParentRoute: () => root });
  const router = createRouter({ routeTree: root.addChildren([Route]), context,
    history: createMemoryHistory({ initialEntries: ["/acme"] }), isServer: server });
  return { preview, request, router, queryClient, options, async dispose() {
    await getDbClient(queryClient).cleanup();
    queryClient.clear();
  } };
}

it("waits for project previews during SSR", async () => {
  const test = setup(true);
  const ensure = vi.spyOn(test.queryClient, "ensureQueryData");
  let complete = false;
  const ready = test.router.load().then(() => { complete = true; });
  await waitFor(() => expect(ensure).toHaveBeenCalled());
  expect(complete).toBe(false);
  test.preview.resolve(true);
  await ready;
  expect(complete).toBe(true);
  await test.dispose();
});

it("commits client navigation while previews load and reuses the warmed cache", async () => {
  const test = setup(false);
  await test.router.load();
  expect(test.queryClient.getQueryState(test.options.queryKey)?.status).toBe("pending");
  test.preview.resolve(true);
  await test.request;
  await test.router.load();
  expect(test.queryClient.getQueryData(test.options.queryKey)).toBe(true);
  await test.dispose();
});

it("preserves preview failures for the content error boundary", async () => {
  const test = setup(false);
  await test.router.load();
  const failure = expect(test.request).rejects.toThrow("Read failed");
  test.preview.reject(new Error("Read failed"));
  await failure;
  expect(test.queryClient.getQueryState(test.options.queryKey)?.status).toBe("error");
  await test.dispose();
});
