// @vitest-environment jsdom
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { createMemoryHistory, createRootRoute, createRouter, RouterProvider } from "@tanstack/react-router";
import { cleanup, render, screen } from "@testing-library/react";
import type { EnvironmentListing } from "@ployz/sdk";
import { afterEach, expect, it, vi } from "vitest";
import * as scopes from "#/collections/use-collection-scope";
import { organizationMembersQueryOptions } from "#/modules/organization/members.queries";
import { OfflineServers, StoreTeardownSection } from "./store-teardown-section";

afterEach(() => { cleanup(); vi.restoreAllMocks(); });

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

it("shows a close waiting for the sweep as deleting, and can delete it again", () => {
  const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false, enabled: false } } });
  vi.spyOn(scopes, "useCollectionScope").mockReturnValue({ queryClient, sessionId: "session", userId: "user" });
  queryClient.setQueryData<unknown>(organizationMembersQueryOptions("acme").queryKey, { members: [{ you: true, role: "owner" }] });
  const feat: EnvironmentListing = {
    id: "id-feat", name: "feat", default: false, parent: "production", merged: false, branch_setup: [], protected: false, protections: 0,
    removal: { kind: "pending", because: "synced", since: 0 },
  };
  render(
    <QueryClientProvider client={queryClient}>
      <StoreTeardownSection organizationSlug="acme" target={{ project: "shop", environment: "feat" }} environments={[feat]}
        name="feat" place="shop/feat" title="Delete feat" description="Deletes feat." actionLabel="Delete feat" headingId="danger"
        onCompleted={() => {}} />
    </QueryClientProvider>,
  );
  expect(screen.getByText("Deleting feat…")).toBeTruthy();
  expect(screen.getByText("Ployz takes it off your servers at its next hourly sweep. If that hasn't started, delete it again.")).toBeTruthy();
  expect(screen.getByRole("button", { name: "Delete feat" }).hasAttribute("disabled")).toBe(false);
});

it.each([
  { environment: "feat", support: true, member: false },
  { environment: null, support: true, member: false },
])("lets support delete what Cloud lets it, and a member not (environment: $environment)", ({ environment, ...expected }) => {
  for (const visitor of ["support", "member"] as const) {
    const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false, enabled: false } } });
    vi.spyOn(scopes, "useCollectionScope").mockReturnValue({ queryClient, sessionId: "session", userId: "user" });
    queryClient.setQueryData(organizationMembersQueryOptions("acme").queryKey, visitor === "support"
      ? { members: [], invites: [], mail: false, support: true }
      : { members: [{ you: true, role: "member", name: "Linus", email: "linus@example.com", joined_at: "2026-01-01T00:00:00Z" }], invites: [], mail: false, support: false });
    render(
      <QueryClientProvider client={queryClient}>
        <StoreTeardownSection organizationSlug="acme" target={{ project: "shop", environment }} environments={[]}
          name="it" place="shop" title="Delete it" description="Deletes it." actionLabel="Delete it" headingId="danger"
          onCompleted={() => {}} />
      </QueryClientProvider>,
    );
    expect(screen.getByRole("button", { name: "Delete it" }).hasAttribute("disabled")).toBe(!expected[visitor]);
    cleanup();
  }
});
