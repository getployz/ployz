// @vitest-environment jsdom
import type { ProjectsView } from "@ployz/sdk";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { createMemoryHistory, createRootRoute, createRouter, RouterProvider } from "@tanstack/react-router";
import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, expect, it, vi } from "vitest";
import * as scopes from "#/collections/use-collection-scope";
import { asTestDouble } from "#/lib/test-double";
import type { StoreResult } from "#/modules/config-store/store.contract";
import { projectsQuery, storeViewOptions } from "#/modules/config-store/store-view.queries";
import { organizationMembersQueryOptions } from "#/modules/organization/members.queries";
import * as lens from "#/modules/runtime/use-runtime-lens";
import * as removal from "#/modules/organization-teardown/organization-removal.functions";
import * as runtime from "#/providers/runtime-provider";
import { StoreOrganizationDanger } from "./store-organization-danger";

afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
});

async function renderAs(role: "owner" | "admin" | "member" | "support") {
  const queryClient = new QueryClient();
  const scope = { queryClient, sessionId: "session", userId: "user" };
  vi.spyOn(scopes, "useCollectionScope").mockReturnValue(scope);
  vi.spyOn(runtime, "useClusterReleased").mockReturnValue(() => {});
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

it("keeps the unconfirmed servers listed when trying again fails, and says why apart from them", async () => {
  vi.spyOn(removal, "removeOrganizationServerFn")
    .mockResolvedValueOnce({ ok: true, value: { organization: "ada", removed: false, servers: { confirmed: [], unconfirmed: ["fra-1"] } } })
    .mockRejectedValueOnce(new Error("Ployz Cloud didn't answer."));
  await renderAs("owner");

  fireEvent.click(await screen.findByRole("button", { name: "Delete organization" }));
  fireEvent.change(await screen.findByRole("textbox", { name: "Type ada to confirm" }), { target: { value: "ada" } });
  await waitFor(() => expect(screen.getByRole("button", { name: "Delete" }).hasAttribute("disabled")).toBe(false));
  await act(async () => { fireEvent.click(screen.getByRole("button", { name: "Delete" })); });
  expect(await screen.findByText(/Some servers didn't confirm they let go of it \(fra-1\)/)).toBeTruthy();

  await act(async () => { fireEvent.click(screen.getByRole("button", { name: "Try again" })); });

  expect(screen.getByText(/Some servers didn't confirm they let go of it \(fra-1\)/)).toBeTruthy();
  expect(screen.getByRole("alert").textContent).toBe("Ployz Cloud didn't answer.");
  expect(screen.getByRole("button", { name: "Try again" }).hasAttribute("disabled")).toBe(false);
});
