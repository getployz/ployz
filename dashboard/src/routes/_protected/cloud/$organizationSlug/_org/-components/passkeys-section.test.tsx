// @vitest-environment jsdom
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { createMemoryHistory, createRootRoute, createRoute, createRouter, RouterProvider } from "@tanstack/react-router";
import { cleanup, render, screen } from "@testing-library/react";
import { Suspense } from "react";
import { afterEach, expect, it, vi } from "vitest";
import * as passkeyFunctions from "#/modules/passkeys/passkeys.functions";
import { PasskeysSection } from "./passkeys-section";

afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
});

async function renderAs(access: "member" | "support") {
  const list = vi.spyOn(passkeyFunctions, "listPasskeysServerFn").mockResolvedValue({ observed: true, passkeys: [] });
  const root = createRootRoute();
  const signedIn = createRoute({ getParentRoute: () => root, id: "_protected" });
  const organization = createRoute({
    getParentRoute: () => signedIn,
    path: "cloud/$organizationSlug",
    loader: () => ({ organizationId: "org-acme", access }),
    component: () => (
      <Suspense fallback="loading">
        <h1>Account</h1>
        <PasskeysSection />
      </Suspense>
    ),
  });
  const router = createRouter({
    routeTree: root.addChildren([signedIn.addChildren([organization])]),
    history: createMemoryHistory({ initialEntries: ["/cloud/acme"] }),
  });
  await router.load();
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  render(<QueryClientProvider client={client}><RouterProvider router={router} /></QueryClientProvider>);
  await screen.findByRole("heading", { name: "Account" });
  return list;
}

it("shows a member their passkeys", async () => {
  const list = await renderAs("member");
  expect(await screen.findByRole("heading", { name: "Passkeys" })).toBeDefined();
  expect(list).toHaveBeenCalled();
});

it("hides passkeys from a support visit without asking for them", async () => {
  const list = await renderAs("support");
  expect(screen.queryByRole("heading", { name: "Passkeys" })).toBeNull();
  expect(list).not.toHaveBeenCalled();
});
