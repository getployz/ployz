// @vitest-environment jsdom
import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { createMemoryHistory, createRootRoute, createRoute, createRouter, RouterProvider } from "@tanstack/react-router";
import type { ConfigCommand, ConfigWritten } from "@ployz/sdk";
import { toast } from "sonner";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import * as scopes from "#/collections/use-collection-scope";
import * as github from "#/modules/github/github.collection";
import { databaseCommand } from "#/modules/config-store/database-presets";
import type { StoreWriteResult } from "#/modules/config-store/store.contract";
import * as functions from "#/modules/config-store/store.functions";
import { useStoreWriter } from "#/modules/config-store/store-write";
import * as creators from "#/routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/-components/canvas/useServiceCreator";
import { ServiceCreateCommand } from "./service-create-command";

// SAFETY: the command reads only the names and the Environment id of a written project.
const projectWritten = { written: "project", project: { name: "brave-otter" }, environment: { id: "env", name: "production" } } as ConfigWritten;
type Answer = () => Promise<StoreWriteResult>;
const saved: Answer = async () => ({ ok: true, value: projectWritten });
const refused: Answer = async () => ({ ok: false, refusal: { code: "conflict", message: "postgres is taken.", details: null } });

function deferred() {
  let settle!: (answer: Answer) => void;
  const answer = new Promise<Answer>((resolve) => { settle = resolve; });
  return { answer: () => answer.then((next) => next()), settle };
}

/** /new over the real Store writer, with the Store's answers to `create_project` and to the database's Batch. */
function setup({ project, database }: { project: Answer; database: Answer }) {
  const queryClient = new QueryClient();
  vi.spyOn(scopes, "useCollectionScope").mockReturnValue({ queryClient, sessionId: "session", userId: "user" });
  vi.spyOn(github, "preloadGithubRepos").mockImplementation(() => {});
  const sent: ConfigCommand["command"][] = [];
  vi.spyOn(functions, "writeStoreServerFn").mockImplementation(async ({ data }) => {
    sent.push(data.command.command);
    return data.command.command === "create_project" ? project() : database();
  });
  vi.spyOn(creators, "useCreateStoreService").mockReturnValue(vi.fn());
  vi.spyOn(creators, "useCreateStoreDatabase").mockImplementation((organizationSlug) => {
    const writer = useStoreWriter(organizationSlug);
    return (target, preset) => ({
      service: { id: "0-0-0-0-db" },
      persisted: writer.commit(databaseCommand(preset, { service: "0-0-0-0-db", volume: "0-0-0-0-vol", environment: target.store,
        name: "postgres", volumeName: "postgres-data" })),
    });
  });
  const toastError = vi.spyOn(toast, "error");

  const root = createRootRoute();
  const page = (path: string, component: () => React.ReactNode = () => null) =>
    createRoute({ getParentRoute: () => root, path, component });
  const router = createRouter({
    routeTree: root.addChildren([
      page("/cloud/$organizationSlug/new", () => <ServiceCreateCommand organizationSlug="acme" initialPanel="database" />),
      page("/cloud/$organizationSlug/~/servers"),
      page("/cloud/$organizationSlug/$projectSlug/$environmentSlug"),
      page("/cloud/$organizationSlug/$projectSlug/$environmentSlug/services/$serviceId"),
    ]),
    history: createMemoryHistory({ initialEntries: ["/cloud/acme/new"] }),
  });
  render(<QueryClientProvider client={queryClient}><RouterProvider router={router} /></QueryClientProvider>);
  const choosePostgres = async () => act(async () => fireEvent.click(await screen.findByRole("option", { name: "PostgreSQL" })));
  return { router, sent, toastError, choosePostgres };
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

it.each([
  ["refused", refused, "postgres is taken."],
  ["unreachable", async () => { throw new TypeError("Failed to fetch"); }, "Couldn't reach Ployz Cloud. Check your connection and try again."],
] satisfies [string, Answer, string][])("a new project whose database is %s opens the project with one toast saying only the database is missing", async (_, database, reason) => {
  const { router, sent, toastError, choosePostgres } = setup({ project: saved, database });

  await choosePostgres();

  await waitFor(() => expect(router.state.location.pathname).toBe("/cloud/acme/brave-otter/production"));
  await waitFor(() => expect(toastError).toHaveBeenCalled());
  expect(toastError.mock.calls).toEqual([
    ["brave-otter was created, but its database wasn’t added. Add it from the canvas.", { description: reason }],
  ]);
  expect(screen.queryByText("Couldn’t create project")).toBeNull();
  expect(sent).toEqual(["create_project", "batch"]);
});

it("a new project whose database fails because the session ended leaves the writer's Sign in toast alone", async () => {
  const { router, toastError, choosePostgres } = setup({
    project: saved,
    database: async () => { throw { _tag: "PublicError", code: "UNAUTHORIZED", message: "Signed out" }; },
  });

  await choosePostgres();

  await waitFor(() => expect(router.state.location.pathname).toBe("/cloud/acme/brave-otter/production"));
  await waitFor(() => expect(toastError).toHaveBeenCalled());
  expect(toastError.mock.calls.map(([message]) => message)).toEqual(["You're signed out. Nothing was saved."]);
});

it("someone who left /new while the database saved stays where they went when it fails", async () => {
  const database = deferred();
  const { router, sent, toastError, choosePostgres } = setup({ project: saved, database: database.answer });

  await choosePostgres();
  await waitFor(() => expect(sent).toEqual(["create_project", "batch"]));
  await act(() => router.navigate({ to: "/cloud/$organizationSlug/~/servers", params: { organizationSlug: "acme" } }));
  await act(async () => database.settle(refused));

  await waitFor(() => expect(toastError).toHaveBeenCalledTimes(1));
  expect(toastError.mock.calls[0]?.[0]).toBe("brave-otter was created, but its database wasn’t added. Add it from the canvas.");
  expect(router.state.location.pathname).toBe("/cloud/acme/~/servers");
});

it("someone who left /new while the database saved stays where they went when it saves", async () => {
  const database = deferred();
  const { router, sent, toastError, choosePostgres } = setup({ project: saved, database: database.answer });

  await choosePostgres();
  await waitFor(() => expect(sent).toEqual(["create_project", "batch"]));
  await act(() => router.navigate({ to: "/cloud/$organizationSlug/~/servers", params: { organizationSlug: "acme" } }));
  await act(async () => database.settle(saved));

  expect(router.state.location.pathname).toBe("/cloud/acme/~/servers");
  expect(toastError).not.toHaveBeenCalled();
});

it("a new project with its database opens the database", async () => {
  const { router, toastError, choosePostgres } = setup({ project: saved, database: saved });

  await choosePostgres();

  await waitFor(() => expect(router.state.location.pathname).toBe("/cloud/acme/brave-otter/production/services/0-0-0-0-db"));
  expect(toastError).not.toHaveBeenCalled();
});

it("a new project whose own write fails stays put and says the project wasn't created", async () => {
  const { router, sent, choosePostgres } = setup({ project: refused, database: saved });

  await choosePostgres();

  expect(await screen.findByText("Couldn’t create project")).toBeTruthy();
  expect(router.state.location.pathname).toBe("/cloud/acme/new");
  expect(sent).toEqual(["create_project"]);
});
