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
  id: "1".repeat(32), name: "web-1", publicIp: null, acceptsIngress: true, endpoints: [], acceptsBuilds: false, acceptsServices: true,
  storage: "pool", buildConcurrency: null, effectiveBuildConcurrency: 1, runningBuilds: 0, daemonVersion: "0.3.0-beta.2", capabilities: [],
  poolCommittableBytes: null, membership: "down", observedContainerCount: 0, observedAt: "2026-10-14T00:00:00Z",
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

it("says a mirrored volume comes back from its mirror, and only a volume with no copy elsewhere is lost", async () => {
  const button = await renderOffline();
  expect(screen.getByText("It's gone. Ployz stops waiting for it. Its volumes come back only where another server holds a mirror.")).toBeTruthy();

  fireEvent.click(button);
  const dialog = await screen.findByRole("alertdialog", { name: "Forget web-1?" });
  expect(dialog.textContent).toContain(
    "A volume with a mirror on another server comes back from it: open the volume's Copies, click Use mirror, then deploy. Volumes with no copy elsewhere are lost.",
  );
  expect(dialog.textContent).not.toContain("can't be recovered");
  expect(dialog.textContent).toContain(
    "web-1 isn't told it was forgotten. If it runs again as it is, its services keep writing to its own copies, and Ployz never brings that back. On 0.3.1 or later it stops them itself a few minutes after it reaches an old peer on 0.3.1 or later; otherwise run sudo ployz-uninstall on it as soon as it boots.",
  );
});

it("warns that the last server, once forgotten, keeps writing to its own copies if it runs again", async () => {
  fireEvent.click(await renderOffline(true));
  const dialog = await screen.findByRole("alertdialog", { name: "Forget all servers?" });
  expect(dialog.textContent).toContain(
    "A forgotten server isn't told it was forgotten. If one runs again as it is, its services keep writing to its own copies, and Ployz never brings that back. On 0.3.1 or later it stops them itself a few minutes after it reaches an old peer on 0.3.1 or later; otherwise run sudo ployz-uninstall on it as soon as it boots.",
  );
});
