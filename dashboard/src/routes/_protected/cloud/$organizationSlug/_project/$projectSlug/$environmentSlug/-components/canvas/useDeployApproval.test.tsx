// @vitest-environment jsdom
import { QueryClient } from "@tanstack/react-query";
import { act, cleanup, renderHook } from "@testing-library/react";
import { afterEach, expect, it, vi } from "vitest";
import * as scopes from "#/collections/use-collection-scope";
import * as prompt from "#/modules/approval-session/approval-prompt";
import * as views from "#/modules/config-store/store-view.queries";
import { applyRuntimeSnapshot, getRuntimeCollections, type RuntimeSnapshot, unreachableRuntimeSnapshot } from "#/modules/runtime/runtime.collection";
import { useDeployApproval } from "./useDeployApproval";

vi.mock("@tanstack/react-router", async (original) => ({
  ...(await original<typeof import("@tanstack/react-router")>()),
  useLoaderData: () => ({ environmentId: "env-1" }),
  useHydrated: () => true,
}));

afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
});

const SCOPE = { environmentId: "env-1", environmentName: "production", namespace: "shop-production", cluster: "key-1", machines: ["m1"] };

const listed: RuntimeSnapshot = {
  status: "observed",
  error: null,
  machines: [{
    id: "m1", name: "fsn-1", publicIp: null, acceptsIngress: true, endpoints: [], acceptsBuilds: true, acceptsServices: true,
    storage: null, buildConcurrency: null, region: null, effectiveBuildConcurrency: 1, runningBuilds: 0, daemonVersion: "0.2.1",
    capabilities: [], poolCommittableBytes: null, sealing: null, membership: "up", unanswered: false, observedContainerCount: 0,
    observedAt: "2026-07-01T00:00:00.000Z",
  }],
  services: [],
  volumeCopies: [],
  certificates: [],
  published: [],
  incompleteIds: { machines: [], containers: [], volumes: [], certificates: [] },
  observedAt: "2026-07-01T00:00:00.000Z",
};

function pressed() {
  const scope = { queryClient: new QueryClient(), sessionId: "session", userId: "user" };
  vi.spyOn(scopes, "useCollectionScope").mockReturnValue(scope);
  // SAFETY: the hook reads only the namespace of the Environment's view.
  vi.spyOn(views, "useCachedStoreView").mockReturnValue({ ok: true, value: { namespace: "shop-production" } } as never);
  const asked = vi.spyOn(prompt, "deployApprovalScope").mockResolvedValue(SCOPE);
  const { result } = renderHook(() => useDeployApproval("acme", { project: "shop", environment: "production" }));
  const lists = (snapshot: RuntimeSnapshot) => act(() => applyRuntimeSnapshot({ collections: getRuntimeCollections("acme", scope), snapshot }));
  return { asked, lists, press: () => result.current.scope() };
}

it("asks for the passkey when Deploy is pressed before Cloud lists the Servers", async () => {
  const { asked, lists, press } = pressed();

  const approval = press();
  lists(listed);

  expect(await approval).toBe(SCOPE);
  expect(asked.mock.calls[0]?.[0].machines.map((machine) => machine.id)).toEqual(["m1"]);
});

it("refuses a Deploy while Cloud can't list the Servers", async () => {
  const { asked, lists, press } = pressed();
  lists(unreachableRuntimeSnapshot("The cluster is expected but unreachable."));

  await expect(press()).rejects.toThrow("Cloud couldn't check the servers. Try again.");
  expect(asked).not.toHaveBeenCalled();
});
