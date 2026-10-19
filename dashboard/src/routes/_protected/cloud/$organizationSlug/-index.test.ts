// @vitest-environment jsdom
import { createMemoryHistory, createRootRoute, createRouter } from "@tanstack/react-router";
import { expect, it } from "vitest";
import { Route } from "./index";

it.each([
  ["/cloud/acme", "/cloud/acme/~"],
  ["/cloud/acme?chat=thread", "/cloud/acme/~?chat=thread"],
])("redirects %s to %s", async (entry, destination) => {
  const root = createRootRoute();
  Object.assign(Route.options, { path: "/cloud/$organizationSlug", getParentRoute: () => root });
  const router = createRouter({
    routeTree: root.addChildren([Route]),
    history: createMemoryHistory({ initialEntries: [entry] }),
    isServer: false,
  });

  await router.load();

  expect(router.state.location.href).toBe(destination);
});
