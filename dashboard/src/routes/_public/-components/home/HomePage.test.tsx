// @vitest-environment jsdom
import { cleanup, render, screen } from "@testing-library/react";
import {
  createMemoryHistory,
  createRootRoute,
  createRouter,
  RouterProvider,
} from "@tanstack/react-router";
import { afterEach, expect, it } from "vitest";
import { HomePage } from "./HomePage";

afterEach(cleanup);

it("renders the features menu, agent section, and sign-up path", async () => {
  const router = createRouter({
    routeTree: createRootRoute({ component: HomePage }),
    history: createMemoryHistory({ initialEntries: ["/"] }),
  });
  render(<RouterProvider router={router} />);
  expect(await screen.findByRole("button", { name: "Features" })).toBeTruthy();
  expect(screen.getByRole("link", { name: /Environment clones/ })).toBeTruthy();
  expect(screen.getByRole("heading", { name: /Give agents real power/ })).toBeTruthy();
  expect(screen.getAllByRole("link", { name: "Climb aboard" })[0]?.getAttribute("href")).toBe("/auth");
  expect(screen.queryByRole("progressbar")).toBeNull();
});
