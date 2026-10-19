// @vitest-environment jsdom
import { createMemoryHistory, createRootRoute, createRouter, RouterProvider } from "@tanstack/react-router";
import { cleanup, render, screen } from "@testing-library/react";
import { afterEach, expect, it } from "vitest";
import { OfflineServers } from "./store-teardown-section";

afterEach(cleanup);

async function show(servers: readonly { id: string; name: string }[]) {
  const router = createRouter({
    routeTree: createRootRoute({ component: () => <p>Deployment #20 failed.<OfflineServers organizationSlug="ada" servers={servers} /></p> }),
    history: createMemoryHistory(),
  });
  await router.load();
  render(<RouterProvider router={router} />);
  return (await screen.findByText(/Deployment #20 failed/)).textContent;
}

it("names an offline Server and links to where it can be forgotten", async () => {
  expect(await show([{ id: "m1", name: "machine-1" }])).toBe("Deployment #20 failed. machine-1 is offline. Bring it back or forget it, then try again.");
  expect(screen.getByRole("link", { name: "machine-1" }).getAttribute("href")).toBe("/cloud/ada/~/servers/m1#remove-server");
});

it("names every offline Server", async () => {
  expect(await show([{ id: "m1", name: "machine-1" }, { id: "m2", name: "machine-2" }]))
    .toBe("Deployment #20 failed. machine-1, machine-2 are offline. Bring them back or forget them, then try again.");
});

it("adds nothing while every Server is online", async () => {
  expect(await show([])).toBe("Deployment #20 failed.");
});
