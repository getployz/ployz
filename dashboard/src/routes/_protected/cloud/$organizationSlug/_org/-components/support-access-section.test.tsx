// @vitest-environment jsdom
import { QueryClientProvider } from "@tanstack/react-query";
import { createMemoryHistory, createRootRoute, createRoute, createRouter, RouterProvider } from "@tanstack/react-router";
import { act, cleanup, render, screen } from "@testing-library/react";
import { afterEach, expect, it, vi } from "vitest";
import * as supportAccess from "#/modules/support-access/support-access.hooks";
import { clientWithRole, gateReason, ROLE_STATES, seedRole } from "#/test/your-role";
import { SupportAccessSection } from "./support-access-section";

afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
});

it.each(ROLE_STATES)("lets only an owner or admin decide what support may do, never support itself (%s)", async (state) => {
  const client = await clientWithRole("ada", state);
  vi.spyOn(supportAccess, "useSupportAccess").mockReturnValue({
    row: { id: "org-ada", allowed: true, variables: false, configs: false, logs: false },
    change: vi.fn(),
  });
  const root = createRootRoute();
  const signedIn = createRoute({ getParentRoute: () => root, id: "_protected" });
  const organization = createRoute({
    getParentRoute: () => signedIn,
    path: "cloud/$organizationSlug",
    loader: () => ({ organizationId: "org-ada", access: state === "support" ? "support" : "member" }),
    component: () => <SupportAccessSection organizationSlug="ada" />,
  });
  const router = createRouter({
    routeTree: root.addChildren([signedIn.addChildren([organization])]),
    history: createMemoryHistory({ initialEntries: ["/cloud/ada"] }),
  });
  await router.load();
  render(<QueryClientProvider client={client}><RouterProvider router={router} /></QueryClientProvider>);
  const open = state === "owner" || state === "admin";
  expect((await screen.findByRole("switch", { name: "Allow support access" })).hasAttribute("data-disabled")).toBe(!open);
  const reason = gateReason(state, "org:settings", undefined);
  if (reason === undefined) expect(screen.queryByText(/your role/)).toBeNull();
  else expect(await screen.findByText(reason, { exact: false })).toBeTruthy();
});

it("keeps working when you go from a support visit to an Organization you belong to", async () => {
  const client = await clientWithRole("acme", "support");
  seedRole(client, "ada", "owner");
  vi.spyOn(supportAccess, "useSupportAccess").mockReturnValue({
    row: { id: "org", allowed: true, variables: false, configs: false, logs: false },
    change: vi.fn(),
  });
  const root = createRootRoute();
  const signedIn = createRoute({ getParentRoute: () => root, id: "_protected" });
  const organization = createRoute({
    getParentRoute: () => signedIn,
    path: "cloud/$organizationSlug",
    loader: ({ params }) => ({ organizationId: `org-${params.organizationSlug}`, access: params.organizationSlug === "acme" ? "support" : "member" }),
    component: function Organization() {
      return <SupportAccessSection organizationSlug={organization.useParams().organizationSlug} />;
    },
  });
  const router = createRouter({
    routeTree: root.addChildren([signedIn.addChildren([organization])]),
    history: createMemoryHistory({ initialEntries: ["/cloud/acme"] }),
  });
  await router.load();
  render(<QueryClientProvider client={client}><RouterProvider router={router} /></QueryClientProvider>);
  expect((await screen.findByRole("switch", { name: "Allow support access" })).hasAttribute("data-disabled")).toBe(true);
  await act(() => router.navigate({ to: "/cloud/$organizationSlug", params: { organizationSlug: "ada" } }));
  await vi.waitFor(() => expect(screen.getByRole("switch", { name: "Allow support access" }).hasAttribute("data-disabled")).toBe(false));
});
