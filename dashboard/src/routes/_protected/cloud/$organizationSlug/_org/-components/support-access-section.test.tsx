// @vitest-environment jsdom
import { QueryClientProvider } from "@tanstack/react-query";
import { createMemoryHistory, createRootRoute, createRoute, createRouter, RouterProvider } from "@tanstack/react-router";
import { cleanup, render, screen } from "@testing-library/react";
import { afterEach, expect, it, vi } from "vitest";
import * as supportAccess from "#/modules/support-access/support-access.hooks";
import { clientWithRole, ROLE_STATES } from "#/test/your-role";
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
});
