// @vitest-environment jsdom
import { cleanup, render, screen } from "@testing-library/react";
import { createMemoryHistory, createRootRoute, createRoute, createRouter, RouterProvider } from "@tanstack/react-router";
import { afterEach, beforeAll, expect, it, vi } from "vitest";
import { RunCanvas } from "./RunCanvas";
import type { RunGraph } from "./run-graph";

beforeAll(() => {
  vi.stubGlobal("ResizeObserver", class { observe() {} unobserve() {} disconnect() {} });
});

afterEach(cleanup);

const graph: RunGraph = {
  nodes: [{ type: "service", id: "api-id", name: "api", light: "deployed", column: 0, row: 0,
    node: { type: "service", id: "api-id", name: "api", outcome: "deployed", rows: [] } }],
  edges: [],
  done: 1,
};

it("lets a mouse click reach a card's link", async () => {
  const root = createRootRoute();
  const deployment = createRoute({
    getParentRoute: () => root, path: "/cloud/$organizationSlug/$projectSlug/$environmentSlug/deployments/$deploymentId",
    component: () => <div style={{ width: 800, height: 600 }}><RunCanvas graph={graph} selectedId={undefined} header={null} /></div>,
  });
  const router = createRouter({
    routeTree: root.addChildren([deployment]),
    history: createMemoryHistory({ initialEntries: ["/cloud/acme/shop/production/deployments/d1"] }),
  });
  render(<RouterProvider router={router} />);
  const card = await screen.findByRole("link", { name: /api/ });
  expect(card.closest<HTMLElement>(".react-flow__node")?.style.pointerEvents).toBe("all");
});
