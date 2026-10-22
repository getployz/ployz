// @vitest-environment jsdom
import { createMemoryHistory, createRootRoute, createRoute, createRouter, RouterProvider } from "@tanstack/react-router";
import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, expect, it, vi } from "vitest";
import { ErrorPage } from "./error-page";

afterEach(() => {
  cleanup();
  vi.useRealTimers();
});

/** A page whose loader fails while `down` holds, as every page does while Cloud's database restarts. */
const app = (down: { value: boolean }) => {
  const root = createRootRoute({
    loader: () => {
      if (down.value) throw new Error("connect ECONNREFUSED 172.28.0.3:5432");
      return { ok: true };
    },
    component: () => <p>Projects</p>,
  });
  const index = createRoute({ getParentRoute: () => root, path: "/" });
  return createRouter({
    routeTree: root.addChildren([index]),
    history: createMemoryHistory({ initialEntries: ["/"] }),
    defaultErrorComponent: ErrorPage,
  });
};

it("says the page couldn't load, keeps the cause out of sight, and comes back by itself once it can", async () => {
  vi.useFakeTimers({ toFake: ["setTimeout", "clearTimeout", "setInterval", "clearInterval"] });
  const down = { value: true };
  render(<RouterProvider router={app(down)} />);
  await act(() => vi.advanceTimersByTimeAsync(0));

  expect(screen.getByText("This page couldn't load")).toBeTruthy();
  expect(screen.queryByText(/ECONNREFUSED/)).toBeNull();

  down.value = false;
  await act(() => vi.advanceTimersByTimeAsync(5_000));
  expect(screen.getByText("Projects")).toBeTruthy();
});

it("tries again at once when asked", async () => {
  const down = { value: true };
  render(<RouterProvider router={app(down)} />);
  const tryAgain = await screen.findByRole("button", { name: "Try again" });
  down.value = false;
  fireEvent.click(tryAgain);
  expect(await screen.findByText("Projects")).toBeTruthy();
});
