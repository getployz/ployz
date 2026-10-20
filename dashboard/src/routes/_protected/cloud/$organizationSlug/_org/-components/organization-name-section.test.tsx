// @vitest-environment jsdom
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { createMemoryHistory, createRootRoute, createRoute, createRouter, RouterProvider } from "@tanstack/react-router";
import { act, cleanup, render, screen } from "@testing-library/react";
import { afterEach, expect, it, vi } from "vitest";
import { membersKey, NO_LONGER_MEMBER } from "#/modules/organization/members.queries";
import * as organizationState from "#/modules/organization/organization-state.queries";
import { clientWithRole, seedRole } from "#/test/your-role";
import { OrganizationNameSection } from "./organization-name-section";

afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
});

it("keeps working when you go from a support visit to an Organization you belong to", async () => {
  const client = await clientWithRole("acme", "support");
  seedRole(client, "ada", "owner");
  vi.spyOn(organizationState, "useOrganizationName").mockReturnValue("Same name");
  const root = createRootRoute();
  const signedIn = createRoute({ getParentRoute: () => root, id: "_protected" });
  const organization = createRoute({
    getParentRoute: () => signedIn,
    path: "cloud/$organizationSlug",
    loader: ({ params }) => ({ organizationId: `org-${params.organizationSlug}`, access: params.organizationSlug === "acme" ? "support" : "member" }),
    component: function Organization() {
      return <OrganizationNameSection organizationSlug={organization.useParams().organizationSlug} />;
    },
  });
  const router = createRouter({
    routeTree: root.addChildren([signedIn.addChildren([organization])]),
    history: createMemoryHistory({ initialEntries: ["/cloud/acme"] }),
  });
  await router.load();
  render(<QueryClientProvider client={client}><RouterProvider router={router} /></QueryClientProvider>);
  expect(await screen.findByRole("textbox", { name: "Display name" })).toHaveProperty("disabled", true);
  await act(() => router.navigate({ to: "/cloud/$organizationSlug", params: { organizationSlug: "ada" } }));
  await vi.waitFor(() => expect(screen.getByRole("textbox", { name: "Display name" })).toHaveProperty("disabled", false));
});

it("says why the name is locked once the members no longer list you", async () => {
  const client = new QueryClient();
  client.setQueryData(membersKey("ada"), {
    members: [{ you: false, role: "owner", name: "Ada", email: "ada@example.com", joined_at: "2026-10-01T00:00:00Z" }],
    invites: [],
    mail: false,
    support: false,
  });
  vi.spyOn(organizationState, "useOrganizationName").mockReturnValue("Same name");
  const root = createRootRoute();
  const signedIn = createRoute({ getParentRoute: () => root, id: "_protected" });
  const organization = createRoute({
    getParentRoute: () => signedIn,
    path: "cloud/$organizationSlug",
    loader: () => ({ organizationId: "org-ada", access: "member" }),
    component: () => <OrganizationNameSection organizationSlug="ada" />,
  });
  const router = createRouter({
    routeTree: root.addChildren([signedIn.addChildren([organization])]),
    history: createMemoryHistory({ initialEntries: ["/cloud/ada"] }),
  });
  await router.load();
  render(<QueryClientProvider client={client}><RouterProvider router={router} /></QueryClientProvider>);
  expect(await screen.findByRole("textbox", { name: "Display name" })).toHaveProperty("disabled", true);
  expect(screen.getByText(`The URL stays the same. ${NO_LONGER_MEMBER}`)).toBeTruthy();
});
