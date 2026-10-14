// @vitest-environment jsdom
import { createMemoryHistory, createRootRoute, createRouter } from "@tanstack/react-router";
import { expect, it } from "vitest";
import { Route } from "./index";

it.each([
  ["acme", "/cloud/acme/~/account"],
  [null, "/cloud"],
])("redirects /cloud/account acting in %s to %s", async (activeOrganizationSlug, destination) => {
  const root = createRootRoute({
    beforeLoad: () => ({
      session: {
        session: { id: "session", userId: "user", activeOrganizationSlug },
        user: { id: "user", name: "User", email: "user@example.test" },
      },
    }),
  });
  Object.assign(Route.options, { path: "/cloud/account", getParentRoute: () => root });
  const router = createRouter({
    routeTree: root.addChildren([Route]),
    history: createMemoryHistory({ initialEntries: ["/cloud/account"] }),
    isServer: false,
  });

  await router.load();

  expect(router.state.location.href).toBe(destination);
});
