// @vitest-environment jsdom
import { QueryClient, QueryClientProvider, focusManager, onlineManager } from "@tanstack/react-query";
import { act, cleanup, renderHook, waitFor } from "@testing-library/react";
import type { DeploymentView, HistoryView } from "@ployz/sdk";
import type { ReactNode } from "react";
import { afterEach, expect, it, vi } from "vitest";
import { applyOrganizationChanges } from "#/collections/org-changes.stream";
import * as scopes from "#/collections/use-collection-scope";
import { asTestDouble } from "#/lib/test-double";
import * as functions from "./store.functions";
import type { StoreResult } from "./store.contract";
import { deploymentQuery, storeViewOptions, useCachedStoreRead, useStoreView } from "./store-view.queries";

// Unmount before restoring the scope spy: a hook left mounted rerenders on a later focus and reads a null router.
afterEach(() => { cleanup(); vi.restoreAllMocks(); });

const query = { query: "history", environment: { project: "shop", environment: "production" } } as const;

type HistoryShown = StoreResult<{ view: "history" } & HistoryView>;

function history(revision: number): HistoryShown {
  return { ok: true, value: {
    view: "history",
    environment: { id: "env", project: "shop", name: "production", revision },
    revisions: [{ revision, message: `Merged #${revision}`, saved_by: null, saved_at: null, predecessor: null, changes: [], total_count: 0, linked: {} }],
    more: false,
    live: null,
    failed: null,
  } };
}

/** An open History tab, and the Store reads it starts, each answered when the test says. */
function openHistory() {
  const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  const scope = { queryClient, sessionId: "session", userId: "user" };
  vi.spyOn(scopes, "useCollectionScope").mockReturnValue(scope);
  const reads: { signal: AbortSignal; answer: (result: HistoryShown) => void }[] = [];
  vi.spyOn(functions, "readStoreViewServerFn").mockImplementation(({ signal }: { signal?: AbortSignal }) =>
    new Promise((answer) => { reads.push({ signal: signal ?? new AbortController().signal, answer }); }) as never);
  queryClient.setQueryData(storeViewOptions("acme", scope, query).queryKey, history(1));
  const wrapper = ({ children }: { children: ReactNode }) => <QueryClientProvider client={queryClient}>{children}</QueryClientProvider>;
  const shown = renderHook(() => useStoreView("acme", query), { wrapper });
  const newest = () => {
    const result = shown.result.current;
    return result.ok ? result.value.revisions[0]?.revision : undefined;
  };
  return { scope, reads, newest, reading: () => queryClient.isFetching() };
}

it("shows a merge while changes keep coming faster than History reads", async () => {
  const tab = openHistory();
  const changed = () => act(() => applyOrganizationChanges(["store_environment", "store_deployment"], "acme", tab.scope));
  // The merge saved revision 2; its Deploy keeps logging changes while History reads.
  changed();
  changed();
  changed();
  await act(async () => tab.reads[0]?.answer(history(2)));
  await waitFor(() => expect(tab.newest()).toBe(2));
  expect(tab.reads[0]?.signal.aborted).toBe(false);

  // That read may have started before the last change committed: one more read follows it, and only one.
  await waitFor(() => expect(tab.reads).toHaveLength(2));
  await act(async () => tab.reads[1]?.answer(history(3)));
  await waitFor(() => expect(tab.newest()).toBe(3));
  expect(tab.reads).toHaveLength(2);
});

it("keeps History's last answer when the Store is busy", async () => {
  const tab = openHistory();
  act(() => applyOrganizationChanges(["store_environment"], "acme", tab.scope));
  await act(async () => tab.reads[0]?.answer({ ok: false, refusal: { code: "unavailable", message: "The Config Store is busy; retry", details: null } }));
  await waitFor(() => expect(tab.reading()).toBe(0));
  expect(tab.newest()).toBe(1);

  // The next change reads again and shows the merge.
  act(() => applyOrganizationChanges(["store_environment"], "acme", tab.scope));
  await waitFor(() => expect(tab.reads).toHaveLength(2));
  await act(async () => tab.reads[1]?.answer(history(2)));
  await waitFor(() => expect(tab.newest()).toBe(2));
});

it("rereads History when only a Deployment changed", async () => {
  const tab = openHistory();
  act(() => applyOrganizationChanges(["store_deployment"], "acme", tab.scope));
  await waitFor(() => expect(tab.reads).toHaveLength(1));
  await act(async () => tab.reads[0]?.answer(history(2)));
  await waitFor(() => expect(tab.newest()).toBe(2));
});

type DeploymentShown = StoreResult<{ view: "deployment" } & DeploymentView>;

function deployment(status: DeploymentView["status"]): DeploymentShown {
  return { ok: true, value: { view: "deployment", ...asTestDouble<DeploymentView>()({
    id: "dep-18", number: 18, saved: 15, status, in_flight: status === "queued" || status === "running",
    environment: { id: "env", project: "shop", name: "production", revision: 15 },
  }) } };
}

it("moves an open Deployment on from Queued when the change stream never says it ran", async () => {
  vi.useFakeTimers();
  try {
    const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } });
    const scope = { queryClient, sessionId: "session", userId: "user" };
    vi.spyOn(scopes, "useCollectionScope").mockReturnValue(scope);
    const answers = [deployment("applied")];
    const read = vi.spyOn(functions, "readStoreViewServerFn").mockImplementation(async () => answers.shift() ?? deployment("applied"));
    const query = deploymentQuery("dep-18");
    queryClient.setQueryData(storeViewOptions("acme", scope, query).queryKey, deployment("queued"));
    const wrapper = ({ children }: { children: ReactNode }) => <QueryClientProvider client={queryClient}>{children}</QueryClientProvider>;
    const shown = renderHook(() => useStoreView("acme", query), { wrapper });
    const status = () => shown.result.current.ok ? shown.result.current.value.status : undefined;
    expect(status()).toBe("queued");

    await act(() => vi.advanceTimersByTimeAsync(10_000));
    expect(status()).toBe("applied");

    const reads = read.mock.calls.length;
    await act(() => vi.advanceTimersByTimeAsync(60_000));
    expect(read.mock.calls.length).toBe(reads);
  } finally {
    vi.useRealTimers();
  }
});

it("tells a chrome view's failed read from an unread one, across rereads, after an answer, and until a read succeeds", async () => {
  const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  const scope = { queryClient, sessionId: "session", userId: "user" };
  vi.spyOn(scopes, "useCollectionScope").mockReturnValue(scope);
  const reads: { fail: () => void; answer: (result: DeploymentShown) => void }[] = [];
  vi.spyOn(functions, "readStoreViewServerFn").mockImplementation(() =>
    new Promise((answer, fail) => { reads.push({ fail: () => fail(new Error("Failed to fetch")), answer }); }) as never);
  const wrapper = ({ children }: { children: ReactNode }) => <QueryClientProvider client={queryClient}>{children}</QueryClientProvider>;
  const shown = renderHook(() => useCachedStoreRead("acme", deploymentQuery("dep-18")), { wrapper });
  const changed = () => act(() => applyOrganizationChanges(["store_deployment"], "acme", scope));
  await waitFor(() => expect(reads).toHaveLength(1));
  expect(shown.result.current).toEqual({ result: undefined, failed: false });

  await act(async () => reads[0]?.fail());
  await waitFor(() => expect(shown.result.current).toEqual({ result: undefined, failed: true }));
  changed();
  await waitFor(() => expect(reads).toHaveLength(2));
  expect(shown.result.current.failed).toBe(true);

  await act(async () => reads[1]?.answer(deployment("running")));
  await waitFor(() => expect(shown.result.current).toMatchObject({ result: { ok: true, value: { status: "running" } }, failed: false }));
  changed();
  await waitFor(() => expect(reads).toHaveLength(3));
  await act(async () => reads[2]?.fail());
  await waitFor(() => expect(shown.result.current).toMatchObject({ result: { ok: true, value: { status: "running" } }, failed: true }));
});

it("leaves no History or Deployment read mounted for a later file's focus or reconnect to rerender", async () => {
  // Runs last: every hook above must already be unmounted, or its rerender reads the real scope and throws.
  focusManager.setFocused(false);
  focusManager.setFocused(true);
  onlineManager.setOnline(false);
  onlineManager.setOnline(true);
  await new Promise((settled) => setTimeout(settled, 200));
  expect(document.body.childElementCount).toBe(0);
});
