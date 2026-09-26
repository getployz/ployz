// @vitest-environment jsdom
import { orgStoreSeed } from "#/test/org-store-tables";
import { Suspense } from "react";
import { act, render, screen, fireEvent, waitFor } from "@testing-library/react";
import { expect, it, vi } from "vitest";
import { QueryClient, QueryClientProvider, dehydrate, hydrate } from "@tanstack/react-query";
import { environmentChangeStateOptions, preloadOrganizationEnvironmentChangeStateProjections, useEnvironmentChangeStates } from "./environment-change-state.queries";
import { applyOrganizationChanges } from "#/collections/org-changes.stream";

import { renderToString } from "react-dom/server";
import { hydrateRoot } from "react-dom/client";
import { getDbClient } from "#/collections/scope";
import { getEnvironmentDeploymentsCollection } from "#/collections/collections";
import { preloadCollection } from "#/collections/query-collection";
import type { EnvironmentChangeStateProjection } from "./deployment-contract";
import { ServiceSettingInput } from "#/routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/services/$serviceId/-components/ServiceSettingInput";

it("hydrates the server projection without a loading fallback or a refetch", async () => {
  const serverClient = new QueryClient();
  const browserClient = new QueryClient();
  const serverScope = { queryClient: serverClient, sessionId: "session", userId: "user" };
  const browserScope = { ...serverScope, queryClient: browserClient };
  const read = vi.fn(async () => comparison("server"));
  await serverClient.fetchQuery(environmentChangeStateOptions("org", serverScope, read));
  await preloadOrganizationEnvironmentChangeStateProjections(serverScope, "org");

  const fallback = vi.fn(() => <span>Loading projection</span>);
  function Projection({ scope }: { scope: typeof serverScope }) {
    const states = useEnvironmentChangeStates("org", scope, read);
    return <span>{states[0]?.applied.token}</span>;
  }
  function App({ scope }: { scope: typeof serverScope }) {
    const Fallback = fallback;
    return <QueryClientProvider client={scope.queryClient}>
      <Suspense fallback={<Fallback />}><Projection scope={scope} /></Suspense>
    </QueryClientProvider>;
  }
  const container = document.createElement("div");
  container.innerHTML = renderToString(<App scope={serverScope} />);
  expect(container.textContent).toBe("server");
  document.body.append(container);
  hydrate(browserClient, dehydrate(serverClient));
  const errors: unknown[] = [];
  let root: ReturnType<typeof hydrateRoot> | undefined;
  try {
    await act(async () => { root = hydrateRoot(container, <App scope={browserScope} />, { onRecoverableError: (error) => errors.push(error) }); });
    expect(container.textContent).toBe("server");
    expect(fallback).not.toHaveBeenCalled();
    expect(errors).toEqual([]);
    expect(browserClient.isFetching()).toBe(0);
    expect(read).toHaveBeenCalledOnce();
  } finally {
    await act(async () => root?.unmount());
    container.remove();
    serverClient.clear();
    browserClient.clear();
  }
});

function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((yes) => { resolve = yes; });
  return { promise, resolve };
}

function comparison(token: string): EnvironmentChangeStateProjection[] {
  return [{ environmentId: "env", saved: null, applied: { token, nodes: [] }, deploymentEvidence: null }];
}

async function editorFixture() {
  const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  const scope = { queryClient, sessionId: "session", userId: "user" };
  const deploymentKey = ["collections", "session", "user", "org", "environment_deployment"];
  const deployment = { id: "deploy", status: "deploying", savedStateSnapshotId: "saved", updatedAt: new Date(0) };
  queryClient.setQueryData(deploymentKey, orgStoreSeed([deployment]));
  await preloadCollection(getEnvironmentDeploymentsCollection("org", scope));
  const read = vi.fn(async () => comparison("initial"));
  const options = environmentChangeStateOptions("org", scope, read);
  await queryClient.fetchQuery(options);
  function Editor() {
    const states = useEnvironmentChangeStates("org", scope, read);
    return <div role="dialog" aria-label="Edit service">
      <span>{states[0]?.applied.token}</span>
      <ServiceSettingInput ariaLabel="Start command" value="npm start" isChanged={false}
        onCommit={() => ({ isPersisted: { promise: Promise.resolve() } })} />
    </div>;
  }
  const view = render(<QueryClientProvider client={queryClient}>
    <Suspense fallback={<p role="status">Loading resource</p>}><Editor /></Suspense>
  </QueryClientProvider>);
  return { queryClient, scope, read, deploymentKey, deployment, options, view,
    /** The change log names the projection: a deployment row or saved revision changed. */
    changed: () => applyOrganizationChanges(["environment_change_state"], "org", scope),
    async dispose() { view.unmount(); await getDbClient(queryClient).cleanup(); queryClient.clear(); } };
}

it("keeps a focused editor stable through progress and refreshes named changes in the background", async () => {
  const test = await editorFixture();
  try {
    const input = screen.getByRole("textbox", { name: "Start command" });
    input.focus();
    fireEvent.change(input, { target: { value: "npm run custom" } });
    // Progress moves deployment rows but never names the projection.
    for (let n = 1; n <= 3; n++) {
      await act(async () => { test.queryClient.setQueryData(test.deploymentKey, orgStoreSeed([{ ...test.deployment, updatedAt: new Date(n) }])); });
    }
    expect(test.read).toHaveBeenCalledTimes(1);
    const applied = deferred<EnvironmentChangeStateProjection[]>();
    const saved = deferred<EnvironmentChangeStateProjection[]>();
    test.read.mockImplementationOnce(() => applied.promise).mockImplementationOnce(() => saved.promise);
    await act(async () => { test.changed(); });
    await waitFor(() => expect(test.read).toHaveBeenCalledTimes(2));
    // A second change during a request supersedes it, so the older response never lands.
    await act(async () => { test.changed(); });
    await waitFor(() => expect(test.read).toHaveBeenCalledTimes(3));
    expect(screen.queryByRole("status")).toBeNull();
    expect(screen.getByText("initial")).toBeTruthy();
    expect(document.activeElement).toBe(input);
    expect(input).toHaveProperty("value", "npm run custom");
    await act(async () => { applied.resolve(comparison("applied")); });
    await act(async () => { saved.resolve(comparison("saved-next")); });
    await screen.findByText("saved-next");
    expect(screen.queryByText("applied")).toBeNull();
    expect(screen.getByRole("textbox", { name: "Start command" })).toBe(input);
    expect(document.activeElement).toBe(input);
    expect(input).toHaveProperty("value", "npm run custom");
    expect(screen.queryByRole("status")).toBeNull();
  } finally { await test.dispose(); }
});

it("refreshes cached comparisons after a named change while the editor is closed", async () => {
  const test = await editorFixture();
  try {
    test.view.unmount();
    const refreshed = deferred<EnvironmentChangeStateProjection[]>();
    test.read.mockImplementationOnce(() => refreshed.promise);
    await act(async () => { test.changed(); });
    function Reopened() {
      const states = useEnvironmentChangeStates("org", test.scope, test.read);
      return <span>{states[0]?.applied.token}</span>;
    }
    test.view = render(<QueryClientProvider client={test.queryClient}>
      <Suspense fallback={<p role="status">Loading</p>}><Reopened /></Suspense>
    </QueryClientProvider>);
    await waitFor(() => expect(test.read).toHaveBeenCalledTimes(2));
    expect(screen.getByText("initial")).toBeTruthy();
    expect(screen.queryByRole("status")).toBeNull();
    await act(async () => { refreshed.resolve(comparison("current")); });
    await screen.findByText("current");
  } finally { test.view.unmount(); await test.dispose(); }
});

it("retains the draft and comparison after a background failure without retrying on keystrokes", async () => {
  const test = await editorFixture();
  try {
    const input = screen.getByRole("textbox", { name: "Start command" });
    input.focus();
    test.read.mockRejectedValueOnce(new Error("temporarily unavailable"));
    await act(async () => { test.changed(); });
    await waitFor(() => expect(test.queryClient.getQueryState(test.options.queryKey)?.status).toBe("error"));
    fireEvent.change(input, { target: { value: "keep typing" } });
    expect(input).toHaveProperty("value", "keep typing");
    expect(document.activeElement).toBe(input);
    expect(screen.getByText("initial")).toBeTruthy();
    expect(screen.queryByRole("status")).toBeNull();
    expect(test.read).toHaveBeenCalledTimes(2);
  } finally { await test.dispose(); }
});
it("serves every environment from one org-wide comparison without refetching", async () => {
  const test = await editorFixture();
  try {
    const readsBefore = test.read.mock.calls.length;
    function OtherEnvironment() {
      const states = useEnvironmentChangeStates("org", test.scope, test.read);
      return <span>{`other:${states[0]?.applied.token}`}</span>;
    }
    test.view.rerender(<QueryClientProvider client={test.queryClient}>
      <Suspense fallback={<p role="status">Loading</p>}><OtherEnvironment /></Suspense>
    </QueryClientProvider>);
    expect(screen.queryByRole("status")).toBeNull();
    expect(screen.getByText("other:initial")).toBeTruthy();
    expect(test.read).toHaveBeenCalledTimes(readsBefore);
    expect(environmentChangeStateOptions("org", { ...test.scope, sessionId: "other-session" }).queryKey)
      .not.toEqual(environmentChangeStateOptions("org", test.scope).queryKey);
  } finally { await test.dispose(); }
});
