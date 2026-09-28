// @vitest-environment jsdom
import { act, screen } from "@testing-library/react";
import { renderToString } from "react-dom/server";
import { createRoot, hydrateRoot } from "react-dom/client";
import { useState } from "react";
import {
  createRootRoute,
  createRoute,
  createRouter,
  createMemoryHistory,
  RouterProvider,
  Outlet,
} from "@tanstack/react-router";
import { hydrate } from "@tanstack/react-router/ssr/client";
import { afterEach, expect, it, vi } from "vitest";
import { getRouter } from "./router";

afterEach(() => vi.unstubAllGlobals());

it("hydrates the initial pending UI for nested client-only routes without replacing the server tree", async () => {
  const appRouter = getRouter();
  const Pending = appRouter.options.defaultPendingComponent;
  appRouter.options.context.queryClient.clear();
  appRouter.history.destroy();

  function create(isServer: boolean) {
    const root = createRootRoute({ component: Outlet });
    const cloud = createRoute({
      getParentRoute: () => root,
      path: "/cloud",
      ssr: false,
      component: Outlet,
    });
    const project = createRoute({
      getParentRoute: () => cloud,
      path: "project",
      loader: () => "ready",
      component: () => <div>Dashboard loaded</div>,
    });
    return createRouter({
      routeTree: root.addChildren([cloud.addChildren([project])]),
      isServer,
      history: createMemoryHistory({ initialEntries: ["/cloud/project"] }),
      defaultPendingComponent: Pending,
    });
  }
  vi.stubGlobal("scrollTo", () => {});
  const server = create(true);
  await server.load();
  const markup = renderToString(<RouterProvider router={server} />);
  expect(server.state.resolvedLocation).toBeDefined();
  expect(markup).toContain("Opening Ployz");
  expect(markup).not.toContain("Loading page");

  // Carry the actual server matches into TanStack's client hydration lane.
  vi.stubGlobal("$_TSR", {
    buffer: [],
    h() {},
    e() {},
    c() {},
    p() {},
    router: {
      manifest: undefined,
      matches: server.state.matches.map((match) => ({
        i: match.id.replaceAll("/", "\0"),
        u: match.updatedAt,
        s: match.status,
        ssr: match.ssr,
        l: match.loaderData,
      })),
    },
  });
  const client = create(false);
  await hydrate(client);
  expect(client.state.resolvedLocation).toBeUndefined();

  const container = document.createElement("div");
  container.innerHTML = markup;
  document.body.append(container);
  const onRecoverableError = vi.fn();
  let root: ReturnType<typeof hydrateRoot> | undefined;
  try {
    await act(async () => {
      root = hydrateRoot(container, <RouterProvider router={client} />, {
        onRecoverableError,
      });
    });
    expect(await screen.findByText("Dashboard loaded")).toBeTruthy();
    expect(onRecoverableError).not.toHaveBeenCalled();
  } finally {
    act(() => root?.unmount());
    container.remove();
    client.clearCache();
    client.history.destroy();
    server.clearCache();
    server.history.destroy();
  }
});

it("starts a page fresh when its params change, but not when a child's do", async () => {
  const appRouter = getRouter();
  const remountDeps = appRouter.options.defaultRemountDeps;
  appRouter.history.destroy();
  const mounts: string[] = [];
  function Mounted({ name, children }: { name: string; children?: React.ReactNode }) {
    const [id] = useState(() => { mounts.push(name); return name; });
    return <>{id}{children}</>;
  }
  const root = createRootRoute({ component: Outlet });
  const environment = createRoute({ getParentRoute: () => root, path: "$environment", component: () => <Mounted name="environment"><Outlet /></Mounted> });
  const service = createRoute({ getParentRoute: () => environment, path: "$service", component: () => <Mounted name="service" /> });
  const router = createRouter({
    routeTree: root.addChildren([environment.addChildren([service])]),
    history: createMemoryHistory({ initialEntries: ["/production/api"] }),
    // SAFETY: the app's rule reads only params, which these routes have too.
    defaultRemountDeps: remountDeps as never,
  });
  vi.stubGlobal("scrollTo", () => {});
  const container = document.createElement("div");
  document.body.append(container);
  let rendered: ReturnType<typeof createRoot> | undefined;
  try {
    await act(async () => { rendered = createRoot(container); rendered.render(<RouterProvider router={router} />); });
    await act(() => router.navigate({ href: "/production/web" }));
    expect(mounts).toEqual(["environment", "service", "service"]);
    await act(() => router.navigate({ href: "/staging/web" }));
    expect(mounts).toEqual(["environment", "service", "service", "environment", "service"]);
  } finally {
    act(() => rendered?.unmount());
    container.remove();
    router.history.destroy();
  }
});
