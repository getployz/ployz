// @vitest-environment jsdom
import { QueryClient } from "@tanstack/react-query";
import { createMemoryHistory, createRootRouteWithContext, createRouter } from "@tanstack/react-router";
import { expect, it } from "vitest";
import { organizationStateQueryOptions } from "#/modules/organization/organization-state.queries";
import { Route } from "./index";

const organizations = (...entries: Array<[slug: string, access: "member" | "support"]>) =>
  entries.map(([slug, access]) => ({ slug, access }));

it.each([
  ["acme", organizations(["beta", "member"]), "/cloud/acme/~/account"],
  // The active Organization was deleted: a support grant is not one of the user's own.
  [null, organizations(["alpha", "support"], ["beta", "member"]), "/cloud/beta/~/account"],
  [null, organizations(["alpha", "support"]), "/cloud/account"],
])("/cloud/account acting in %s with %j lands on %s", async (activeOrganizationSlug, orgs, destination) => {
  const queryClient = new QueryClient();
  queryClient.setQueryData(organizationStateQueryOptions().queryKey, { organizations: orgs } as never);
  const root = createRootRouteWithContext<{ queryClient: QueryClient }>()({
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
    context: { queryClient },
    isServer: false,
  });

  await router.load();

  expect(router.state.location.href).toBe(destination);
});
