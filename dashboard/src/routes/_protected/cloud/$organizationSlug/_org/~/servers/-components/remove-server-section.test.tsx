// @vitest-environment jsdom
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { createMemoryHistory, createRootRoute, createRouter, RouterProvider } from "@tanstack/react-router";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, expect, it, vi } from "vitest";
import type { RuntimeMachineRecord } from "#/modules/runtime/runtime.collection";
import * as runtime from "#/providers/runtime-provider";
import { RemoveServerSection } from "./remove-server-section";

afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
});

const machine: RuntimeMachineRecord = {
  id: "1".repeat(32), name: "web-1", subnet: null, joinedAt: null, publicIp: null, acceptsIngress: true, endpoints: [], acceptsBuilds: false, acceptsServices: true,
  storage: "pool", buildConcurrency: null, region: null, effectiveBuildConcurrency: 1, runningBuilds: 0, daemonVersion: "0.3.0-beta.2", capabilities: [],
  poolCommittableBytes: null, sealing: null, membership: "down", unanswered: false, observedContainerCount: 0, observedAt: "2026-10-14T00:00:00Z",
};

async function renderOffline(last = false) {
  vi.spyOn(runtime, "useClusterReleased").mockReturnValue(() => {});
  const root = createRootRoute({
    component: () => <RemoveServerSection machine={machine} organizationSlug="ada" last={last} offline hint={null} gate={{ allowed: true, reason: undefined }} />,
  });
  const router = createRouter({ routeTree: root, history: createMemoryHistory({ initialEntries: ["/"] }) });
  render(<QueryClientProvider client={new QueryClient()}><RouterProvider router={router} /></QueryClientProvider>);
  return screen.findByRole("button", { name: "Forget server" });
}

it("says what forgetting does, and that volumes only on the server may be lost when it can't list them", async () => {
  fireEvent.click(await renderOffline());
  const dialog = await screen.findByRole("alertdialog", { name: "Forget web-1?" });
  expect(dialog.textContent).toContain("web-1 will be removed from your cluster. Any volumes only on web-1 will be lost.");
  expect(dialog.textContent).not.toContain("Use mirror");
});
