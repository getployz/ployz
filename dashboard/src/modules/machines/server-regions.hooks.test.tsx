// @vitest-environment jsdom
import type { ConfigView, OwnedNamespace } from "@ployz/sdk";
import { onlineManager, QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { act, cleanup, renderHook, waitFor } from "@testing-library/react";
import type { ReactNode } from "react";
import { afterEach, expect, it, vi } from "vitest";
import * as scopes from "#/collections/use-collection-scope";
import type { StoreResult } from "#/modules/config-store/store.contract";
import * as functions from "#/modules/config-store/store.functions";
import { storeViewPrefix } from "#/modules/config-store/store-view.queries";
import { getDbClient } from "#/collections/scope";
import { applyRuntimeSnapshot, getRuntimeCollections, type RuntimeIncompleteIds, type RuntimeMachineRecord } from "#/modules/runtime/runtime.collection";
import { useNewEnvironmentRegion, useOwnedNamespaces } from "./server-regions.hooks";

afterEach(() => { cleanup(); vi.restoreAllMocks(); });

const shop: OwnedNamespace = { namespace: "shop-production", project: "shop", environment: "production", region: "default" };

const namespaces = (...owned: OwnedNamespace[]): StoreResult<ConfigView> => ({ ok: true, value: { view: "namespaces", namespaces: owned } });

/** The Server page's read of the Environments' Namespaces, each read answered as the test says. */
function openServerPage() {
  const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  vi.spyOn(scopes, "useCollectionScope").mockReturnValue({ queryClient, sessionId: "session", userId: "user" });
  const reads: { answer: (result: StoreResult<ConfigView>) => void; fail: (error: Error) => void }[] = [];
  vi.spyOn(functions, "readStoreViewServerFn").mockImplementation(() =>
    new Promise((answer, fail) => { reads.push({ answer, fail }); }) as never);
  const wrapper = ({ children }: { children: ReactNode }) => <QueryClientProvider client={queryClient}>{children}</QueryClientProvider>;
  const shown = renderHook(() => useOwnedNamespaces("acme"), { wrapper });
  const refresh = () => act(() => { void queryClient.invalidateQueries({ queryKey: storeViewPrefix("acme") }); });
  return { reads, kind: () => shown.result.current.shown.kind, now: () => shown.result.current.now().kind, refresh };
}

it("holds the picker while Environments' Namespaces are read again, and when that read fails", async () => {
  const page = openServerPage();
  expect(page.kind()).toBe("reading");
  await waitFor(() => expect(page.reads).toHaveLength(1));
  await act(async () => page.reads[0]?.answer(namespaces(shop)));
  await waitFor(() => expect(page.kind()).toBe("read"));

  page.refresh();
  await waitFor(() => expect(page.reads).toHaveLength(2));
  expect(page.kind(), "the answer shown may be from before a change").toBe("reading");

  await act(async () => page.reads[1]?.fail(new Error("offline")));
  await waitFor(() => expect(page.kind()).toBe("failed"));
});

it("shows the Namespaces again once the read that failed succeeds", async () => {
  const page = openServerPage();
  await waitFor(() => expect(page.reads).toHaveLength(1));
  await act(async () => page.reads[0]?.answer(namespaces(shop)));
  page.refresh();
  await waitFor(() => expect(page.reads).toHaveLength(2));
  await act(async () => page.reads[1]?.fail(new Error("offline")));
  await waitFor(() => expect(page.kind()).toBe("failed"));

  page.refresh();
  await waitFor(() => expect(page.reads).toHaveLength(3));
  await act(async () => page.reads[2]?.answer(namespaces(shop)));
  await waitFor(() => expect(page.kind()).toBe("read"));
});

it("holds the picker when a read asked again waits for the network, though an earlier answer is cached", async () => {
  const page = openServerPage();
  await waitFor(() => expect(page.reads).toHaveLength(1));
  await act(async () => page.reads[0]?.answer(namespaces(shop)));
  await waitFor(() => expect(page.kind()).toBe("read"));
  try {
    act(() => onlineManager.setOnline(false));
    page.refresh();
    await waitFor(() => expect(page.kind()).toBe("reading"));
  } finally {
    onlineManager.setOnline(true);
  }
});

const founder: RuntimeMachineRecord = {
  id: "1".repeat(32), name: "fsn-1", subnet: "10.210.0.0/24", joinedAt: null, publicIp: null, acceptsIngress: true, endpoints: [], acceptsBuilds: false,
  acceptsServices: true, storage: "pool", buildConcurrency: null, region: "default", effectiveBuildConcurrency: 1, runningBuilds: 0,
  daemonVersion: "0.4.0-beta.2", capabilities: [], poolCommittableBytes: null, sealing: null, membership: "up", unanswered: false,
  observedContainerCount: 0, observedAt: "2026-11-19T00:00:00Z",
};

async function newEnvironmentRegion(unread: Partial<RuntimeIncompleteIds>) {
  const queryClient = new QueryClient();
  const scope = { queryClient, sessionId: "session", userId: "user" };
  vi.spyOn(scopes, "useCollectionScope").mockReturnValue(scope);
  applyRuntimeSnapshot({
    collections: getRuntimeCollections("acme", scope),
    snapshot: {
      status: "observed", error: null, machines: [founder], services: [], volumeCopies: [], certificates: [], published: [],
      incompleteIds: { machines: [], containers: [], volumes: [], certificates: [], ...unread }, observedAt: founder.observedAt,
    },
  });
  const shown = renderHook(() => useNewEnvironmentRegion("acme"));
  try {
    await waitFor(() => expect(shown.result.current.settled).toBe(true));
    return shown.result.current.region;
  } finally {
    shown.unmount();
    await getDbClient(queryClient).cleanup();
  }
}

it("starts a new Environment in the first Server's region though a Container went unread", async () => {
  expect(await newEnvironmentRegion({ containers: ["c".repeat(64)] })).toBe("default");
});

it("gives a new Environment no region while a Machine went unread", async () => {
  expect(await newEnvironmentRegion({ machines: ["2".repeat(32)] })).toBeNull();
});
