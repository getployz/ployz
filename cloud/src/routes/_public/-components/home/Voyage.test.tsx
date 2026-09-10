// @vitest-environment jsdom
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import {
  createMemoryHistory,
  createRootRoute,
  createRouter,
  RouterProvider,
} from "@tanstack/react-router";
import { afterEach, expect, it, vi } from "vitest";
import { Voyage } from "./Voyage";

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
});

it("rewards each discovery once, completes in any order, and resets the adventure", async () => {
  vi.stubGlobal("scrollTo", () => {});
  const router = createRouter({
    routeTree: createRootRoute({ component: Voyage }),
    history: createMemoryHistory({ initialEntries: ["/"] }),
  });
  render(<RouterProvider router={router} />);
  fireEvent.click(await screen.findByRole("button", { name: /Open waters/ }));
  fireEvent.click(screen.getByRole("button", { name: "Chart your course" }));
  expect(screen.getByRole("progressbar").getAttribute("value")).toBe("1");
  fireEvent.click(screen.getByRole("button", { name: /Open waters/ }));
  fireEvent.click(screen.getByRole("button", { name: "Keep exploring" }));
  expect(screen.getByRole("progressbar").getAttribute("value")).toBe("1");
  fireEvent.click(screen.getByRole("button", { name: "Claim your ship" }));
  fireEvent.click(
    screen.getByRole("button", { name: "Explore the possibilities" }),
  );
  expect(screen.getByRole("progressbar").getAttribute("value")).toBe("3");
  expect(
    screen.getByRole("link", { name: "Climb aboard" }).getAttribute("href"),
  ).toBe("/auth");
  expect(
    screen.getByRole("heading", { name: "Captain of your own ship." }),
  ).toBeTruthy();
  fireEvent.click(screen.getByRole("button", { name: "Reset adventure" }));
  expect(screen.getByRole("progressbar").getAttribute("value")).toBe("0");
  expect(screen.getByRole("button", { name: "Claim your ship" })).toBeTruthy();
});
