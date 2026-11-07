// @vitest-environment jsdom
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { createMemoryHistory, createRootRoute, createRoute, createRouter, RouterProvider } from "@tanstack/react-router";
import { cleanup, render, screen } from "@testing-library/react";
import { Suspense } from "react";
import { afterEach, expect, it, vi } from "vitest";
import * as passkeyFunctions from "#/modules/passkeys/passkeys.functions";
import { PasskeyList, PasskeysSection } from "./passkeys-section";

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

it("keeps the steps, sign-in first, until every server trusts the passkey", () => {
  const passkey = { code: "q3x7mfk2pa", account: "ada@example.com", createdAt: "2026-11-07T09:00:00.000Z" };
  render(<PasskeyList view={{ observed: true, passkeys: [{ ...passkey, servers: [
    { server: "web-1", approval: { by: "server" } },
    { server: "web-2", approval: null },
  ] }] }} />);
  expect(screen.getByText("Trusted on 1 of 2 servers")).toBeDefined();
  expect(screen.getByText("web-2").nextSibling?.textContent).toBe("Not yet approved");
  expect(screen.getByText("sudo ployz login")).toBeDefined();
  expect(screen.getByText("sudo ployz device approve --passkey q3x7mfk2pa")).toBeDefined();
  cleanup();

  render(<PasskeyList view={{ observed: true, passkeys: [{ ...passkey, servers: [
    { server: "web-1", approval: { by: "server" } },
    { server: "web-2", approval: { by: "device", code: "dddddddddd" } },
  ] }] }} />);
  expect(screen.getByText("Trusted on every server")).toBeDefined();
  expect(screen.getByText("Approved by device dddddddddd")).toBeDefined();
  expect(screen.queryByText("sudo ployz login")).toBeNull();
});
