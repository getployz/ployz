// @vitest-environment jsdom
import type { ProjectsView } from "@ployz/sdk";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { createMemoryHistory, createRootRoute, createRouter, RouterProvider } from "@tanstack/react-router";
import { cleanup, render, screen } from "@testing-library/react";
import { afterEach, expect, it, vi } from "vitest";
import * as scopes from "#/collections/use-collection-scope";
import { asTestDouble } from "#/lib/test-double";
import type { StoreResult } from "#/modules/config-store/store.contract";
import { projectsQuery, storeViewOptions } from "#/modules/config-store/store-view.queries";
import { organizationMembersQueryOptions } from "#/modules/organization/members.queries";
import * as lens from "#/modules/runtime/use-runtime-lens";
import { StoreOrganizationDanger } from "./store-organization-danger";

afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
});

async function renderAs(role: "owner" | "admin" | "member" | "support") {
  const queryClient = new QueryClient();
  const scope = { queryClient, sessionId: "session", userId: "user" };
  vi.spyOn(scopes, "useCollectionScope").mockReturnValue(scope);
  vi.spyOn(lens, "useRuntimeLens").mockReturnValue(asTestDouble<ReturnType<typeof lens.useRuntimeLens>>()({ machines: [] }));
  const noProjects: StoreResult<{ view: "projects" } & ProjectsView> = { ok: true, value: { view: "projects", projects: [] } };
  queryClient.setQueryData(storeViewOptions("ada", scope, projectsQuery()).queryKey, noProjects);
  queryClient.setQueryData(organizationMembersQueryOptions("ada").queryKey, {
    members: role === "support" ? [] : [{ name: "Linus", email: "linus@example.com", role, joined_at: "2026-01-01T00:00:00Z", you: true }],
    invites: [],
    mail: false,
    support: role === "support",
  });
  const router = createRouter({
    routeTree: createRootRoute({ component: () => <StoreOrganizationDanger organizationSlug="ada" /> }),
    history: createMemoryHistory(),
  });
  await router.load();
  render(<QueryClientProvider client={queryClient}><RouterProvider router={router} /></QueryClientProvider>);
}

const disabled = async (name: string) => (await screen.findByRole("button", { name })).hasAttribute("disabled");

it("disables Forget servers and Delete organization for a member, and says why", async () => {
  await renderAs("member");

  expect(await disabled("Forget servers")).toBe(true);
  expect(await disabled("Delete organization")).toBe(true);
  expect(screen.getByText(/Only an owner or admin can forget them\./)).toBeTruthy();
  expect(screen.getByText(/Only an owner can delete it\./)).toBeTruthy();
});

it("lets an admin forget servers but not delete the organization", async () => {
  await renderAs("admin");

  expect(await disabled("Forget servers")).toBe(false);
  expect(await disabled("Delete organization")).toBe(true);
});

it("lets an owner do both", async () => {
  await renderAs("owner");

  expect(await disabled("Forget servers")).toBe(false);
  expect(await disabled("Delete organization")).toBe(false);
});

it("lets support forget servers but not delete the organization, as Cloud does", async () => {
  await renderAs("support");

  expect(await disabled("Forget servers")).toBe(false);
  expect(await disabled("Delete organization")).toBe(true);
});
