// @vitest-environment jsdom
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { act, renderHook, waitFor } from "@testing-library/react";
import type { HistoryView } from "@ployz/sdk";
import type { ReactNode } from "react";
import { afterEach, expect, it, vi } from "vitest";
import { applyOrganizationChanges } from "#/collections/org-changes.stream";
import * as scopes from "#/collections/use-collection-scope";
import * as functions from "./store.functions";
import type { StoreResult } from "./store.contract";
import { storeViewOptions, useStoreView } from "./store-view.queries";

afterEach(() => vi.restoreAllMocks());

const query = { query: "history", environment: { project: "shop", environment: "production" } } as const;

type HistoryShown = StoreResult<{ view: "history" } & HistoryView>;

function history(revision: number): HistoryShown {
  return { ok: true, value: {
    view: "history",
    environment: { id: "env", project: "shop", name: "production", revision },
    revisions: [{ revision, message: `Merged #${revision}`, saved_by: null, saved_at: null, predecessor: null, changes: [], total_count: 0, linked: {} }],
    more: false,
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
