// @vitest-environment jsdom
import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { createMemoryHistory, createRootRoute, createRoute, createRouter, RouterProvider } from "@tanstack/react-router";
import type { ConfigWritten } from "@ployz/sdk";
import { toast } from "sonner";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import * as scopes from "#/collections/use-collection-scope";
import * as github from "#/modules/github/github.collection";
import * as storeWrite from "#/modules/config-store/store-write";
import * as creators from "#/routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/-components/canvas/useServiceCreator";
import { ServiceCreateCommand } from "./service-create-command";

// SAFETY: the command reads only the names and the Environment id of a written project.
const projectWritten = { written: "project", project: { name: "brave-otter" }, environment: { id: "env", name: "production" } } as ConfigWritten;
type Write = () => Promise<ConfigWritten>;

function setup({ project, database }: { project: Write; database: Write }) {
  const queryClient = new QueryClient();
  vi.spyOn(scopes, "useCollectionScope").mockReturnValue({ queryClient, sessionId: "session", userId: "user" });
  vi.spyOn(github, "preloadGithubRepos").mockImplementation(() => {});
  const commit = vi.fn(project);
  // SAFETY: the command only commits through the writer.
  vi.spyOn(storeWrite, "useStoreWriter").mockReturnValue({ commit } as never);
  const createDatabase = vi.fn(() => ({ service: { id: "0-0-0-0-db" as const }, persisted: database() }));
  vi.spyOn(creators, "useCreateStoreService").mockReturnValue(vi.fn());
  vi.spyOn(creators, "useCreateStoreDatabase").mockReturnValue(createDatabase);
  const toastError = vi.spyOn(toast, "error");

  const root = createRootRoute();
  const page = (path: string, component: () => React.ReactNode = () => null) =>
    createRoute({ getParentRoute: () => root, path, component });
  const router = createRouter({
    routeTree: root.addChildren([
      page("/cloud/$organizationSlug/new", () => <ServiceCreateCommand organizationSlug="acme" initialPanel="database" />),
      page("/cloud/$organizationSlug/$projectSlug/$environmentSlug"),
      page("/cloud/$organizationSlug/$projectSlug/$environmentSlug/services/$serviceId"),
    ]),
    history: createMemoryHistory({ initialEntries: ["/cloud/acme/new"] }),
  });
  render(<QueryClientProvider client={queryClient}><RouterProvider router={router} /></QueryClientProvider>);
  return { router, commit, createDatabase, toastError };
}

beforeEach(() => {
  vi.stubGlobal("ResizeObserver", class { observe() {} unobserve() {} disconnect() {} });
  Element.prototype.scrollIntoView ??= () => {};
});

afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
});

it("a new project whose database fails to save opens the project and says only the database is missing", async () => {
  const { router, commit, toastError } = setup({
    project: async () => projectWritten,
    database: async () => { throw new Error("refused"); },
  });

  await act(async () => fireEvent.click(await screen.findByRole("option", { name: "PostgreSQL" })));

  await waitFor(() => expect(router.state.location.pathname).toBe("/cloud/acme/brave-otter/production"));
  expect(toastError).toHaveBeenCalledWith(expect.stringContaining("brave-otter was created, but its database wasn’t added"));
  expect(screen.queryByText("Couldn’t create project")).toBeNull();
  expect(commit).toHaveBeenCalledTimes(1);
});

it("a new project with its database opens the database", async () => {
  const { router, toastError } = setup({ project: async () => projectWritten, database: async () => projectWritten });

  await act(async () => fireEvent.click(await screen.findByRole("option", { name: "PostgreSQL" })));

  await waitFor(() => expect(router.state.location.pathname).toBe("/cloud/acme/brave-otter/production/services/0-0-0-0-db"));
  expect(toastError).not.toHaveBeenCalled();
});

it("a new project whose own write fails stays put and says the project wasn't created", async () => {
  const { router, createDatabase } = setup({
    project: async () => { throw new Error("refused"); },
    database: async () => projectWritten,
  });

  await act(async () => fireEvent.click(await screen.findByRole("option", { name: "PostgreSQL" })));

  expect(await screen.findByText("Couldn’t create project")).toBeTruthy();
  expect(router.state.location.pathname).toBe("/cloud/acme/new");
  expect(createDatabase).not.toHaveBeenCalled();
});
