// @vitest-environment jsdom
import { cleanup, render, screen } from "@testing-library/react";
import { createMemoryHistory, createRootRoute, createRoute, createRouter, RouterProvider } from "@tanstack/react-router";
import { afterEach, beforeAll, expect, it, vi } from "vitest";
import { detourLane, RunCanvas } from "./RunCanvas";
import type { RunGraph, RunNode } from "#/modules/config-store/deployments/run-graph";

beforeAll(() => {
  vi.stubGlobal("ResizeObserver", class { observe() {} unobserve() {} disconnect() {} });
});

afterEach(cleanup);

const graph: RunGraph = {
  nodes: [{ type: "service", id: "api-id", name: "api", light: "deployed", column: 0, row: 0,
    node: { type: "service", id: "api-id", name: "api", outcome: "deployed", rows: [] } }],
  edges: [],
  done: 1,
  total: 1,
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

const service = (name: string, column: number, row: number): RunNode => ({
  type: "service", id: name, name, light: "deployed", column, row,
  node: { type: "service", id: name, name, outcome: "deployed", rows: [] },
});

it("routes an edge that skips a column clear of the cards it skips, and leaves adjacent edges straight", () => {
  const graphOf = (nodes: RunNode[]): RunGraph => ({ nodes, edges: [], done: 0, total: 0 });
  const clear = (lane: number | undefined, topRow: number, bottomRow: number) =>
    lane !== undefined && (lane <= topRow * 96 || lane >= bottomRow * 96 + 72);

  const chain = graphOf([service("db", 1, 0), service("api", 2, 0), service("web", 3, 0)]);
  expect(detourLane(chain, { source: "db", target: "api" })).toBeUndefined();
  expect(detourLane(chain, { source: "api", target: "web" })).toBeUndefined();
  expect(clear(detourLane(chain, { source: "db", target: "web" }), 0, 0)).toBe(true);

  const stacked = graphOf([service("db", 1, 0), service("api", 2, -0.5), service("cache", 2, 0.5), service("web", 3, 0)]);
  expect(clear(detourLane(stacked, { source: "db", target: "web" }), -0.5, 0.5)).toBe(true);
});
