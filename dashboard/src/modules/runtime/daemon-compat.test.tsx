// @vitest-environment jsdom
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { DbProvider } from "@tanstack/react-db";
import { createMemoryHistory, createRootRoute, createRoute, createRouter, RouterContextProvider } from "@tanstack/react-router";
import { act, cleanup, render, screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import { getDbClient } from "#/collections/scope";
import { lacks, requirementFor, requirements, ServerLacks, serverSupports, useServerSupports } from "./daemon-compat";
import { asSdkFailure, PloyzProviderError } from "./ployz.server";
import { getRuntimeCollections, type RuntimeMachineRecord } from "./runtime.collection";

const machine = (id: string, capabilities: readonly string[]): RuntimeMachineRecord => ({
  id, name: id, publicIp: null, acceptsIngress: true, endpoints: [], acceptsBuilds: true, acceptsServices: true, storage: "pool",
  buildConcurrency: null, effectiveBuildConcurrency: 1, runningBuilds: 0, daemonVersion: "0.2.4", capabilities, membership: "active",
  observedContainerCount: 0, observedAt: "2026-10-12T00:00:00Z",
});

describe("lacks", () => {
  it("splits the Servers that named a capability from every other failure", () => {
    const ordinary = { machineId: "fsn-1", machineName: "fsn-1", message: "Its log service isn’t running." };
    const split = lacks([
      { machineId: "hel-1", machineName: "hel-1", message: "unsupported", code: "unsupported", capability: "ployz.logs.history.v2" },
      ordinary,
      { machineId: "nbg-1", machineName: "nbg-1", message: "refused", code: "unsupported" },
    ]);
    expect(split.lacks).toEqual([new ServerLacks({ machineId: "hel-1", capability: "ployz.logs.history.v2" })]);
    expect(split.rest.map(failure => failure.machineId)).toEqual(["fsn-1", "nbg-1"]);
  });
});

describe("serverSupports", () => {
  it("reads the Server's record, and claims nothing when the record lists no capabilities", () => {
    expect(serverSupports({ capabilities: [requirements.liveLogs.capability] }, requirements.liveLogs)).toBe(true);
    expect(serverSupports({ capabilities: [requirements.liveLogs.capability] }, requirements.volumeMove)).toBe(false);
    expect(serverSupports({ capabilities: [] }, requirements.volumeMove)).toBe("unknown");
  });

  it("names the feature a refused capability belongs to", () => {
    expect(requirementFor("ployz.logs.tail.v1")).toBe(requirements.liveLogs);
    expect(requirementFor("ployz.something.v9")).toBeNull();
  });
});

describe("asSdkFailure", () => {
  it("turns a refusal that names a capability into ServerLacks for the Server it addressed", () => {
    const failure = asSdkFailure("logs", { code: "unsupported", message: "", details: { capability: "ployz.logs.tail.v1", target: "machine-1" } });
    expect(failure).toEqual(new ServerLacks({ machineId: "machine-1", capability: "ployz.logs.tail.v1" }));
  });

  it("keeps an unsupported refusal without a capability as the provider's own failure", () => {
    const failure = asSdkFailure("logs", { code: "unsupported", message: "disabled", details: { cause: "disabled" } });
    expect(failure).toBeInstanceOf(PloyzProviderError);
  });
});

describe("useServerSupports", () => {
  it("is unknown until the Server's record arrives, then follows it", async () => {
    const client = new QueryClient();
    const root = createRootRoute();
    const protectedRoute = createRoute({ getParentRoute: () => root, id: "_protected", beforeLoad: () => ({ session: { session: { id: "session" }, user: { id: "user" } } }) });
    const index = createRoute({ getParentRoute: () => protectedRoute, path: "/" });
    const router = createRouter({ routeTree: root.addChildren([protectedRoute.addChildren([index])]), history: createMemoryHistory({ initialEntries: ["/"] }) });
    await router.load();
    const machines = getRuntimeCollections("supports", { queryClient: client, sessionId: "session", userId: "user" }).machines;
    function Probe() {
      return <output>{String(useServerSupports("supports", "machine-1", requirements.volumeMove))}</output>;
    }
    try {
      render(<QueryClientProvider client={client}><DbProvider client={getDbClient(client)}><RouterContextProvider router={router}><Probe /></RouterContextProvider></DbProvider></QueryClientProvider>);
      expect(screen.getByRole("status").textContent).toBe("unknown");
      await act(async () => { machines.insert(machine("machine-1", [requirements.liveLogs.capability])); });
      expect(screen.getByRole("status").textContent).toBe("false");
      await act(async () => { machines.update("machine-1", draft => { draft.capabilities = [requirements.volumeMove.capability]; }); });
      expect(screen.getByRole("status").textContent).toBe("true");
    } finally {
      cleanup();
      await getDbClient(client).cleanup(); client.clear();
    }
  });
});
