// @vitest-environment jsdom
import { createMemoryHistory, createRootRoute, createRoute, createRouter, RouterProvider } from "@tanstack/react-router";
import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, expect, it, vi } from "vitest";
import { ErrorPage } from "./error-page";

afterEach(() => {
  cleanup();
  vi.useRealTimers();
});

/** A page whose loader fails while `down` holds, as every page does while Cloud's database restarts. Once up, each load
 * counts in `slow.starts` and waits for `slow.gate`, when given. */
const app = (down: { value: boolean }, slow?: { starts: number; gate: Promise<void> }) => {
  const root = createRootRoute({
    loader: async () => {
      if (down.value) throw new Error("connect ECONNREFUSED 172.28.0.3:5432");
      if (slow) {
        slow.starts += 1;
        await slow.gate;
      }
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

  expect(screen.getByRole("heading", { level: 1, name: "This page couldn't load" })).toBeTruthy();
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

it("lets a load slower than the retry finish instead of starting it over", async () => {
  vi.useFakeTimers({ toFake: ["setTimeout", "clearTimeout", "setInterval", "clearInterval"] });
  const down = { value: true };
  let open = () => {};
  const slow = { starts: 0, gate: new Promise<void>((resolve) => { open = resolve; }) };
  render(<RouterProvider router={app(down, slow)} />);
  await act(() => vi.advanceTimersByTimeAsync(0));
  down.value = false;
  // The first try starts a load that outlasts the next two ticks.
  await act(() => vi.advanceTimersByTimeAsync(3 * 5_000));
  expect(slow.starts).toBe(1);
  open();
  await act(() => vi.advanceTimersByTimeAsync(0));
  expect(screen.getByText("Projects")).toBeTruthy();
});
