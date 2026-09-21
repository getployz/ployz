// @vitest-environment jsdom

import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import {
  createMemoryHistory, createRootRoute, createRoute, createRouter, Link, Outlet,
  RouterProvider, useParams, useSearch,
} from "@tanstack/react-router";
import { Schema } from "effect";
import { use } from "react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { CanvasInspectorHeader } from "./CanvasInspectorHeader";
import { CanvasInspectorOverlay } from "./CanvasInspectorOverlay";
import { CanvasInspectorError } from "./CanvasInspectorRouteStates";
import { ENVIRONMENT_SERVICE_ROUTE_TO } from "./environment-route-paths";
import { serviceSearchSchema } from "../services/$serviceId/-components/service-pages";
import { CanvasNodeList } from "./canvas/CanvasServiceList";
import type { CanvasEnvironmentResourceState, CanvasVolumeResourceState } from "./canvas/CanvasServicesContext";
import { asTestDouble } from "#/lib/test-double";

let workspaceWidth = 1000;
const params = { organizationSlug: "acme", projectSlug: "shop", environmentSlug: "production" };
const volume = asTestDouble<CanvasVolumeResourceState>()({
  diffRowCount: 1,
  resource: { resource: { id: "data", name: "Database data" }, isAuthored: true, attachments: [{ mountPath: "/data" }] },
});
const variableGroup = asTestDouble<CanvasEnvironmentResourceState>()({
  diffRowCount: 0,
  resource: { resource: { id: "shared", name: "Shared variables" }, exports: [{ key: "DATABASE_URL" }] },
});

function InspectorEditor() {
  const routeParams = useParams({ strict: false });
  const search = useSearch({ strict: false });
  return <div>
    <CanvasInspectorHeader params={{
      organizationSlug: routeParams.organizationSlug ?? "",
      projectSlug: routeParams.projectSlug ?? "",
      environmentSlug: routeParams.environmentSlug ?? "",
    }}><button>Edit service name</button></CanvasInspectorHeader>
    <p>{search.tab === "variables" ? "Environment variables" : "Configuration"}</p>
    <input aria-label="Draft setting" defaultValue="initial" />
    <button onKeyDown={(event) => { if (event.key === "Escape") event.preventDefault(); }}>Nested menu</button>
  </div>;
}

function Architecture() {
  const routeParams = useParams({ strict: false });
  const nodeId = routeParams.serviceId ?? routeParams.resourceId ?? null;
  return <CanvasInspectorOverlay
    selection={nodeId ? {
      key: `${routeParams.organizationSlug}/${routeParams.projectSlug}/${routeParams.environmentSlug}/${nodeId}`,
      nodeId,
    } : null}
    header={<header>Architecture</header>}
    canvas={<div className="canvas-graph" role="region" aria-label="Mobile architecture list">
      <Link data-canvas-node="api" to={ENVIRONMENT_SERVICE_ROUTE_TO} params={{ ...params, serviceId: "api" }}>API node</Link>
      <CanvasNodeList
        services={[]}
        servicesById={new Map()}
        selectedNodeId={nodeId}
        volumeResourcesById={new Map([["data", volume]])}
        environmentResourcesById={new Map([["shared", variableGroup]])}
      />
    </div>}
  >
    <Outlet />
  </CanvasInspectorOverlay>;
}

async function openInspector(state: "ready" | "pending" | "error" = "ready") {
  const loadingResource = new Promise<void>(() => {});
  const root = createRootRoute({ component: Outlet });
  const protectedRoute = createRoute({ getParentRoute: () => root, id: "_protected", component: Outlet });
  const organizationRoute = createRoute({ getParentRoute: () => protectedRoute, path: "cloud/$organizationSlug", component: Outlet });
  const projectGroup = createRoute({ getParentRoute: () => organizationRoute, id: "_project", component: Outlet });
  const environment = createRoute({ getParentRoute: () => projectGroup, path: "$projectSlug/$environmentSlug", component: Outlet });
  const canvas = createRoute({ getParentRoute: () => environment, id: "_canvas", component: Architecture });
  const index = createRoute({ getParentRoute: () => canvas, path: "/", component: () => null });
  const service = createRoute({
    getParentRoute: () => canvas,
    path: "services/$serviceId",
    validateSearch: Schema.toStandardSchemaV1(serviceSearchSchema),
    component: state === "pending"
      ? () => { use(loadingResource); return null; }
      : state === "error"
        ? () => { throw new Error("Resource failed to load"); }
        : InspectorEditor,
    errorComponent: () => <CanvasInspectorError noun="Service" />,
  });
  const resource = createRoute({ getParentRoute: () => canvas, path: "resources/$resourceId", component: InspectorEditor });
  const routeTree = root.addChildren([protectedRoute.addChildren([organizationRoute.addChildren([
    projectGroup.addChildren([environment.addChildren([canvas.addChildren([index, service, resource])])]),
  ])])]);
  const router = createRouter({
    routeTree,
    history: createMemoryHistory({ initialEntries: ["/cloud/acme/shop/production/services/api"] }),
  });
  render(<RouterProvider router={router} />);
  if (state === "ready") await screen.findByLabelText("Draft setting");
  else if (state === "pending") await screen.findByRole("status", { name: "Loading resource" });
  else await screen.findByText("Service couldn’t load");
  return router;
}

beforeEach(() => {
  workspaceWidth = 1000;
  vi.stubGlobal("innerWidth", 1200);
  vi.stubGlobal("scrollTo", () => {});
  vi.stubGlobal("matchMedia", () => ({ matches: false, addEventListener() {}, removeEventListener() {} }));
  vi.spyOn(HTMLElement.prototype, "getBoundingClientRect").mockImplementation(() => ({
    width: workspaceWidth, height: 700, top: 0, left: 0, bottom: 700, right: workspaceWidth,
    x: 0, y: 0, toJSON: () => ({}),
  }));
});
afterEach(() => { cleanup(); vi.restoreAllMocks(); vi.unstubAllGlobals(); });

describe("canvas inspector presentation", () => {
  it("keeps the canvas and editor when filling, changing pages, and restoring", async () => {
    const router = await openInspector();
    const canvasNode = screen.getByText("API node");
    const input = screen.getByLabelText<HTMLInputElement>("Draft setting");
    fireEvent.change(input, { target: { value: "unsaved edit" } });
    expect(screen.getByRole("link", { name: "Close inspector" })).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "Fill canvas" }));
    expect(screen.queryByRole("link", { name: "Close inspector" })).toBeNull();
    expect(screen.getByRole("link", { name: "Back to Architecture" })).toBeTruthy();
    await act(() => router.navigate({
      to: ENVIRONMENT_SERVICE_ROUTE_TO,
      params: { ...params, serviceId: "api" },
      search: { tab: "variables" },
    }));
    expect(screen.getByText("Environment variables")).toBeTruthy();
    expect(screen.getByRole("button", { name: "Restore inspector" })).toBeTruthy();
    expect(screen.getByLabelText("Draft setting")).toBe(input);
    expect(input.value).toBe("unsaved edit");
    fireEvent.click(screen.getByRole("button", { name: "Restore inspector" }));
    expect(screen.getByRole("link", { name: "Close inspector" })).toBeTruthy();
    expect(screen.getByText("API node")).toBe(canvasNode);
  });

  it("resets explicit fullscreen on resource or scope changes", async () => {
    const router = await openInspector();
    fireEvent.click(screen.getByRole("button", { name: "Fill canvas" }));
    expect(screen.getByRole("button", { name: "Restore inspector" })).toBeTruthy();
    await act(() => router.navigate({ to: ENVIRONMENT_SERVICE_ROUTE_TO, params: { ...params, serviceId: "worker" } }));
    expect(screen.getByRole("button", { name: "Fill canvas" })).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "Fill canvas" }));
    await act(() => router.navigate({ to: ENVIRONMENT_SERVICE_ROUTE_TO, params: { ...params, organizationSlug: "other", serviceId: "worker" } }));
    expect(screen.getByRole("button", { name: "Fill canvas" })).toBeTruthy();
    await act(() => router.navigate({ to: ENVIRONMENT_SERVICE_ROUTE_TO, params: { ...params, serviceId: "api" } }));
    expect(screen.getByRole("button", { name: "Fill canvas" })).toBeTruthy();
  });

  it("returns direct links to Architecture and restores focus and mobile-list scroll", async () => {
    const router = await openInspector();
    const canvasNode = screen.getByText("API node");
    const list = screen.getByRole("region", { name: "Mobile architecture list" });
    list.scrollTop = 215;
    expect(screen.getByRole("link", { name: "Close inspector" }).getAttribute("href")).toBe("/cloud/acme/shop/production");
    fireEvent.click(screen.getByRole("link", { name: "Close inspector" }));
    await waitFor(() => expect(router.state.location.pathname).toBe("/cloud/acme/shop/production"));
    expect(screen.getByText("API node")).toBe(canvasNode);
    expect(screen.getByRole("region", { name: "Mobile architecture list" })).toBe(list);
    expect(list.scrollTop).toBe(215);
    expect(document.activeElement).toBe(canvasNode);
  });

  it("renders responsive return, rename, and page navigation without viewport measurement", async () => {
    vi.stubGlobal("innerWidth", 390);
    await openInspector();
    expect(screen.getByRole("button", { name: "Edit service name" })).toBeTruthy();
    expect(screen.getByRole("link", { name: "Close inspector" }).className).toContain("canvas-inspector-close");
    expect(screen.getByRole("link", { name: "Back to Architecture" }).className).toContain("canvas-inspector-back");
    expect(screen.getAllByRole("button", { name: "Project navigation" })).toHaveLength(1);
    expect(screen.getByRole("button", { name: "Project navigation" }).textContent).toBe("Settings");
    expect(screen.getByRole("button", { name: "Fill canvas" }).hasAttribute("data-canvas-inspector-resize")).toBe(true);
  });

  it.each([
    ["pending", 1200], ["pending", 390], ["error", 1200], ["error", 390],
  ] as const)("keeps the return control usable for %s resources at %ipx", async (state, width) => {
    vi.stubGlobal("innerWidth", width);
    const router = await openInspector(state);
    const returnControl = screen.getByRole("link", {
      name: width <= 860 ? "Back to Architecture" : "Close inspector",
    });
    expect(returnControl.getAttribute("href")).toBe("/cloud/acme/shop/production");
    fireEvent.click(returnControl);
    await waitFor(() => expect(router.state.location.pathname).toBe("/cloud/acme/shop/production"));
    expect(screen.queryByRole("region", { name: "Resource inspector" })).toBeNull();
    expect(document.activeElement).toBe(screen.getByText("API node"));
  });

  it("exposes supported Volume and Variable Group editors in the mobile Architecture list", async () => {
    vi.stubGlobal("innerWidth", 390);
    const router = await openInspector();
    expect(screen.getByText("Volume")).toBeTruthy();
    expect(screen.getByText("Variable group")).toBeTruthy();
    fireEvent.click(screen.getByRole("link", { name: /Database data/ }));
    await waitFor(() => expect(router.state.location.pathname).toBe("/cloud/acme/shop/production/resources/data"));
    expect(screen.getByRole("link", { name: /Database data/ }).getAttribute("aria-current")).toBe("page");
    fireEvent.click(screen.getByRole("link", { name: /Shared variables/ }));
    await waitFor(() => expect(router.state.location.pathname).toBe("/cloud/acme/shop/production/resources/shared"));
  });

  it("closes on Escape inside the inspector after nested controls have handled it", async () => {
    const router = await openInspector();
    fireEvent.keyDown(document.body, { key: "Escape" });
    fireEvent.keyDown(screen.getByRole("button", { name: "Nested menu" }), { key: "Escape" });
    expect(router.state.location.pathname).toBe("/cloud/acme/shop/production/services/api");
    fireEvent.keyDown(screen.getByLabelText("Draft setting"), { key: "Escape" });
    await waitFor(() => expect(router.state.location.pathname).toBe("/cloud/acme/shop/production"));
    expect(document.activeElement).toBe(screen.getByText("API node"));
  });
});
