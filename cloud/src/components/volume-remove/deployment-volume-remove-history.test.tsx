// @vitest-environment jsdom

import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import {
  Outlet,
  RouterProvider,
  createMemoryHistory,
  createRootRoute,
  createRoute,
  createRouter,
} from "@tanstack/react-router";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { EnvironmentDeploymentSummary } from "#/modules/deployments/deployment-contract";
import { VolumeRemoveAttemptHistory } from "./deployment-volume-remove-history";

type Attempt = EnvironmentDeploymentSummary["volumeRemoveAttempts"][number];

const routeParams = {
  organizationSlug: "acme",
  projectSlug: "web",
  environmentSlug: "production",
};

function volumeRemoveAttempt(overrides: Partial<Attempt> = {}): Attempt {
  return {
    id: "attempt-1",
    environmentDeploymentId: "deployment-1",
    environmentResourceId: "volume-1",
    retryOfAttemptId: null,
    volumes: [{ machine_id: "machine-1", name: "pg-data" }],
    status: "unknown",
    inngestRunId: "run-1",
    outcome: null,
    failureMessage: "Cloud did not receive the outcome.",
    startedAt: new Date("2026-09-08T00:00:00Z"),
    terminalAt: new Date("2026-09-08T00:01:00Z"),
    createdAt: new Date("2026-09-08T00:00:00Z"),
    updatedAt: new Date("2026-09-08T00:01:00Z"),
    ...overrides,
  };
}

async function renderHistory(input: {
  attempts: Attempt[];
  availableVolumeResourceIds: ReadonlySet<string>;
}) {
  const rootRoute = createRootRoute({ component: Outlet });
  const historyRoute = createRoute({
    getParentRoute: () => rootRoute,
    path: "/history",
    component: () => (
      <VolumeRemoveAttemptHistory
        {...routeParams}
        attempts={input.attempts}
        availableVolumeResourceIds={input.availableVolumeResourceIds}
      />
    ),
  });
  const volumeRoute = createRoute({
    getParentRoute: () => rootRoute,
    path: "/cloud/$organizationSlug/$projectSlug/$environmentSlug/resources/$resourceId",
    component: () => <p>Volume drawer</p>,
  });
  const router = createRouter({
    routeTree: rootRoute.addChildren([historyRoute, volumeRoute]),
    history: createMemoryHistory({ initialEntries: ["/history"] }),
  });

  await router.load();
  render(<RouterProvider router={router} />);
}

beforeEach(() => {
  vi.stubGlobal("scrollTo", vi.fn());
});

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
});

describe("VolumeRemoveAttemptHistory", () => {
  it("opens the existing volume drawer to recover an available attempt", async () => {
    await renderHistory({
      attempts: [volumeRemoveAttempt()],
      availableVolumeResourceIds: new Set(["volume-1"]),
    });

    const recoveryLink = await screen.findByRole("button", {
      name: "Review volume removal",
    });
    expect(recoveryLink.getAttribute("href")).toBe(
      "/cloud/acme/web/production/resources/volume-1",
    );

    fireEvent.click(recoveryLink);
    expect(await screen.findByText("Volume drawer")).toBeTruthy();
  });

  it("keeps completed history without linking to its deleted volume row", async () => {
    await renderHistory({
      attempts: [
        volumeRemoveAttempt({
          status: "completed",
          outcome: { destroyed: [{ machine_id: "machine-1", name: "pg-data" }], failed: [], omitted: [] },
          failureMessage: null,
        }),
      ],
      availableVolumeResourceIds: new Set(),
    });

    expect(await screen.findByText("completed")).toBeTruthy();
    expect(
      screen.queryByRole("button", { name: "Review volume removal" }),
    ).toBeNull();
  });
});
