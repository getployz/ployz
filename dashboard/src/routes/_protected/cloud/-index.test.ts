// @vitest-environment jsdom
import { createMemoryHistory, createRootRoute, createRouter } from "@tanstack/react-router";
import { expect, it } from "vitest";
import { Route } from "./index";

it.each([
  ["/cloud?welcome=true", "/cloud/acme/new"],
  ["/cloud", "/cloud/acme/~"],
  ["/cloud?welcome=false", "/cloud/acme/~"],
])("redirects %s to %s", async (entry, destination) => {
  const root = createRootRoute({
    beforeLoad: () => ({
      session: {
        session: { id: "session", userId: "user", activeOrganizationSlug: "acme" },
        user: { id: "user", name: "User", email: "user@example.test" },
      },
    }),
  });
  Object.assign(Route.options, { path: "/cloud", getParentRoute: () => root });
  const router = createRouter({
    routeTree: root.addChildren([Route]),
    history: createMemoryHistory({ initialEntries: [entry] }),
    isServer: false,
  });

  await router.load();

  expect(router.state.location.href).toBe(destination);
});
