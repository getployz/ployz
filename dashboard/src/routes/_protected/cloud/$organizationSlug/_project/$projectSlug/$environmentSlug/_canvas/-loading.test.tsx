// @vitest-environment jsdom
import { waitFor } from "@testing-library/react";
import { QueryClient, environmentManager } from "@tanstack/react-query";
import { createMemoryHistory, createRootRouteWithContext, createRouter } from "@tanstack/react-router";
import { afterEach, expect, it, vi } from "vitest";
import { getEnvironmentDeploymentsCollection, getEnvironmentSavedStateRevisionsCollection, getEnvironmentNodeIntroductionsCollection } from "#/collections/collections";
import { environmentChangeStateOptions } from "#/modules/deployments/use-environment-state-projection";
import { environmentCanvasOptions, environmentResourcesOptions } from "#/modules/environment-design/environment-data";
import { Route } from "./route";

afterEach(() => { vi.restoreAllMocks(); });

function setup(server: boolean) {
  vi.spyOn(environmentManager, "isServer").mockReturnValue(server);
  let resolve!: (value: boolean) => void;
  let reject!: (error: Error) => void;
  const promise = new Promise<boolean>((yes, no) => { resolve = yes; reject = no; });
  const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  const scope = { queryClient, sessionId: "session", userId: "user", environmentSlug: "production" };
  const params = { organizationSlug: "acme", projectSlug: "app", environmentSlug: "production" };
  for (const table of ["environment_deployment", "environment_saved_state_snapshot", "environment_node_introduction"]) {
    queryClient.setQueryData(["collections", "session", "user", "acme", table, "production"], []);
  }
  queryClient.setQueryData(environmentChangeStateOptions("acme", scope).queryKey, { version: "", states: [] });
  // Seed an in-flight read through the real Query cache; the loader must share it.
  const request = queryClient.fetchQuery({ ...environmentResourcesOptions(params, scope), queryFn: () => promise });
  const context = { queryClient, session: { session: { id: "session" }, user: { id: "user" } } };
  const root = createRootRouteWithContext<typeof context>()();
  Object.assign(Route.options, { path: "/$organizationSlug/$projectSlug/$environmentSlug", getParentRoute: () => root });
  const router = createRouter({
    routeTree: root.addChildren([Route]), context,
    history: createMemoryHistory({ initialEntries: ["/acme/app/production"] }),
    isServer: server,
  });
  async function dispose() {
    await Promise.all([
      getEnvironmentDeploymentsCollection("acme", scope).cleanup(),
      getEnvironmentSavedStateRevisionsCollection("acme", scope).cleanup(),
      getEnvironmentNodeIntroductionsCollection("acme", scope).cleanup(),
    ]);
    queryClient.clear();
  }
  return { resolve, reject, request, router, queryClient, dispose, options: environmentCanvasOptions(params, scope) };
}

it("waits for canvas data during SSR", async () => {
  const test = setup(true);
  const ensure = vi.spyOn(test.queryClient, "ensureQueryData");
  let completed = false;
  const ready = test.router.load().then(() => { completed = true; });
  await waitFor(() => expect(ensure).toHaveBeenCalled());
  expect(completed).toBe(false);
  test.resolve(true);
  await ready;
  expect(completed).toBe(true);
  await test.dispose();
});

it("commits a client route before data is ready, then populates the shared query", async () => {
  const test = setup(false);
  await test.router.load();
  expect(test.router.state.matches.at(-1)?.loaderData).toBeUndefined();
  expect(test.queryClient.getQueryState(test.options.queryKey)?.status).toBe("pending");
  test.resolve(true);
  await waitFor(() => expect(test.queryClient.getQueryData(test.options.queryKey)).toBe(true));
  await test.dispose();
});

it("preserves deferred client failures for the content error boundary", async () => {
  const test = setup(false);
  await test.router.load();
  const failure = expect(test.queryClient.ensureQueryData(test.options)).rejects.toThrow("Environment unavailable");
  const requestFailure = expect(test.request).rejects.toThrow("Environment unavailable");
  test.reject(new Error("Environment unavailable"));
  await Promise.all([failure, requestFailure]);
  await test.dispose();
});
