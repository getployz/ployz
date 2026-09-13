// @vitest-environment jsdom

import {
  cleanup,
  fireEvent,
  render,
  screen,
  waitFor,
} from "@testing-library/react";
import {
  Outlet,
  RouterProvider,
  createMemoryHistory,
  createRootRoute,
  createRoute,
  createRouter,
} from "@tanstack/react-router";
import { afterEach, describe, expect, it, vi } from "vitest";
import { WireframeContent } from "./wireframe";

const scrollRestorationSelector =
  '[data-scroll-restoration-id="wireframe-content"]';
const originalElementScrollTo = Object.getOwnPropertyDescriptor(
  HTMLElement.prototype,
  "scrollTo",
);

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
  if (originalElementScrollTo) {
    Object.defineProperty(
      HTMLElement.prototype,
      "scrollTo",
      originalElementScrollTo,
    );
  } else {
    Reflect.deleteProperty(HTMLElement.prototype, "scrollTo");
  }
});

describe("WireframeContent", () => {
  it("resets its persistent surface when navigating to a new child route", async () => {
    vi.stubGlobal("scrollTo", vi.fn());
    Object.defineProperty(HTMLElement.prototype, "scrollTo", {
      configurable: true,
      value({ top = 0, left = 0 }: ScrollToOptions) {
        this.scrollTop = top;
        this.scrollLeft = left;
      },
    });

    const rootRoute = createRootRoute({
      component: () => (
        <WireframeContent surfaceClassName="overflow-y-auto">
          <Outlet />
        </WireframeContent>
      ),
    });
    const deploymentsRoute = createRoute({
      getParentRoute: () => rootRoute,
      path: "/deployments",
      component: () => <div>Deployments</div>,
    });
    const settingsRoute = createRoute({
      getParentRoute: () => rootRoute,
      path: "/settings",
      component: () => <div>Settings</div>,
    });
    const router = createRouter({
      routeTree: rootRoute.addChildren([deploymentsRoute, settingsRoute]),
      history: createMemoryHistory({ initialEntries: ["/deployments"] }),
      scrollRestoration: true,
      scrollToTopSelectors: [scrollRestorationSelector],
    });

    await router.load();
    const { container } = render(<RouterProvider router={router} />);
    const surface = container.querySelector<HTMLElement>(".overflow-y-auto");

    expect(surface).not.toBeNull();
    if (!surface) {
      throw new Error("Wireframe content surface not found");
    }
    surface.scrollTop = 640;
    fireEvent.scroll(surface);

    router.history.push("/settings");
    await screen.findByText("Settings");

    await waitFor(() => expect(surface.scrollTop).toBe(0));
  });
});
