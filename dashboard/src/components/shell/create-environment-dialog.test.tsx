// @vitest-environment jsdom
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { createMemoryHistory, createRootRoute, createRoute, createRouter, RouterProvider } from "@tanstack/react-router";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import type { ConfigCommand } from "@ployz/sdk";
import { afterEach, expect, it, vi } from "vitest";
import * as scopes from "#/collections/use-collection-scope";
import * as functions from "#/modules/config-store/store.functions";
import * as regionHooks from "#/modules/machines/server-regions.hooks";
import { CreateEnvironmentDialog } from "./create-environment-dialog";

afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
});

async function open(servers: ReturnType<typeof regionHooks.useNewEnvironmentRegion>) {
  const queryClient = new QueryClient();
  vi.spyOn(scopes, "useCollectionScope").mockReturnValue({ queryClient, sessionId: "session", userId: "user" });
  vi.spyOn(regionHooks, "useNewEnvironmentRegion").mockReturnValue(servers);
  const commands: ConfigCommand[] = [];
  vi.spyOn(functions, "writeStoreServerFn").mockImplementation(async ({ data }) => {
    commands.push(data.command);
    return { ok: true, value: { written: "environment", environment: { id: "e", name: "staging" } } } as never;
  });
  const root = createRootRoute();
  const router = createRouter({
    routeTree: root.addChildren([
      createRoute({ getParentRoute: () => root, path: "/", component: () => <CreateEnvironmentDialog onOpenChange={vi.fn()} organizationSlug="acme" projectSlug="shop" /> }),
    ]),
    history: createMemoryHistory({ initialEntries: ["/"] }),
  });
  render(<QueryClientProvider client={queryClient}><RouterProvider router={router} /></QueryClientProvider>);
  fireEvent.change(await screen.findByRole("textbox", { name: "Name" }), { target: { value: "staging" } });
  return commands;
}

const add = () => screen.getByRole<HTMLButtonElement>("button", { name: "Add environment" });

it("starts the Environment in the region of the Cluster's first Server", async () => {
  const commands = await open({ settled: true, region: "fsn" });
  fireEvent.click(add());
  await waitFor(() => expect(commands[0]).toMatchObject({ command: "create_environment", project: "shop", name: "staging", region: "fsn" }));
});

it("sends no region for a Cluster that pins none", async () => {
  const commands = await open({ settled: true, region: null });
  fireEvent.click(add());
  await waitFor(() => expect(commands).toHaveLength(1));
  expect(commands[0]).not.toHaveProperty("region");
});

it("waits for the Servers to be read rather than make an Environment that could be anywhere", async () => {
  const commands = await open({ settled: false, region: null });
  expect(add().disabled).toBe(true);
  fireEvent.submit(add().closest("form") as HTMLFormElement);
  await Promise.resolve();
  expect(commands).toEqual([]);
});
