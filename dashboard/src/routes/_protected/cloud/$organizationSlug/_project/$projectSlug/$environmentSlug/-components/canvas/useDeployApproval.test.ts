import { QueryClient } from "@tanstack/react-query";
import { expect, it, vi } from "vitest";
import { applyRuntimeSnapshot, getRuntimeCollections, type RuntimeSnapshot, unreachableRuntimeSnapshot } from "#/modules/runtime/runtime.collection";
import { pressedScope } from "./useDeployApproval";

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
  const collections = getRuntimeCollections("acme", { queryClient: new QueryClient(), sessionId: "session", userId: "user" });
  const asked = vi.fn<Parameters<typeof pressedScope>[2] & {}>(() => Promise.resolve(SCOPE));
  return {
    asked,
    lists: (snapshot: RuntimeSnapshot) => applyRuntimeSnapshot({ collections, snapshot }),
    press: () => pressedScope(collections, { environmentId: "env-1", environmentName: "production", namespace: "shop-production" }, asked),
  };
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
