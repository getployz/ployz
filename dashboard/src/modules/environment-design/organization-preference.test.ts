// @vitest-environment jsdom
import { QueryClient } from "@tanstack/react-query";
import { createMemoryHistory, createRootRoute, createRoute, createRouter } from "@tanstack/react-router";
import { expect, it, vi } from "vitest";
import { rememberSelectedOrganization } from "./workspace.queries";
import type { syncOrganizationSlugServerFn } from "./workspace-functions";

it("persists committed organization changes, never preloads, and retries failed writes", async () => {
  const client = new QueryClient();
  const select = vi.fn<typeof syncOrganizationSlugServerFn>().mockResolvedValue({ organizationId: "id", organizationSlug: "other" });
  const root = createRootRoute();
  const persist = ({ params }: { params: { organizationSlug: string } }) => {
    void rememberSelectedOrganization(client, params.organizationSlug, "acme", select);
  };
  const organization = createRoute({ getParentRoute: () => root, path: "/cloud/$organizationSlug", onEnter: persist, onStay: persist });
  const router = createRouter({ routeTree: root.addChildren([organization]), history: createMemoryHistory({ initialEntries: ["/cloud/acme"] }), isServer: false });
  try {
    await router.load();
    await router.preloadRoute({ to: "/cloud/$organizationSlug", params: { organizationSlug: "other" } });
    expect(select).not.toHaveBeenCalled();
    await router.navigate({ to: "/cloud/$organizationSlug", params: { organizationSlug: "other" } });
    await vi.waitFor(() => expect(client.getQueryData(["organization-preference"])).toBe("other"));
    select.mockRejectedValueOnce(new Error("Offline"));
    await router.navigate({ to: "/cloud/$organizationSlug", params: { organizationSlug: "acme" } });
    await vi.waitFor(() => expect(client.isMutating()).toBe(0));
    expect(client.getQueryData(["organization-preference"])).toBe("other");
    await rememberSelectedOrganization(client, "acme", "acme", select);
    expect(client.getQueryData(["organization-preference"])).toBe("acme");
    expect(select).toHaveBeenCalledTimes(3);
  } finally { client.clear(); }
});
