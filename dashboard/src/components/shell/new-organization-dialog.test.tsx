// @vitest-environment jsdom
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { createMemoryHistory, createRootRoute, createRouter, RouterProvider } from "@tanstack/react-router";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, expect, it, vi } from "vitest";
import * as organizationFunctions from "#/modules/organization/organization-state.functions";
import { NewOrganizationDialog } from "./new-organization-dialog";

afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
});

it("creates one organization when Create is double-clicked", async () => {
  const create = vi.spyOn(organizationFunctions, "createOrganizationServerFn").mockReturnValue(new Promise(() => {}));
  const router = createRouter({
    routeTree: createRootRoute({ component: () => <NewOrganizationDialog onOpenChange={vi.fn()} /> }),
    history: createMemoryHistory({ initialEntries: ["/"] }),
  });
  await router.load();
  render(<QueryClientProvider client={new QueryClient()}><RouterProvider router={router} /></QueryClientProvider>);

  fireEvent.change(await screen.findByLabelText("Name"), { target: { value: "Acme" } });
  const submit = screen.getByRole("button", { name: "Create" });
  fireEvent.click(submit);
  fireEvent.click(submit);

  await waitFor(() => expect(create).toHaveBeenCalled());
  expect(create).toHaveBeenCalledTimes(1);
});
