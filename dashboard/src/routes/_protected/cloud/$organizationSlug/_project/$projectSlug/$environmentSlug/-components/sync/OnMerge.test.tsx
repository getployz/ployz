// @vitest-environment jsdom
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import {
  createMemoryHistory, createRootRoute, createRoute, createRouter, Outlet, RouterProvider,
} from "@tanstack/react-router";
import type { BranchView, ConfigCommand, ConfigQuery, ConfigView, MergeRow, OnMerge, PrPlansView, PullRequestView, RowId } from "@ployz/sdk";
import { toast } from "sonner";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import * as scopes from "#/collections/use-collection-scope";
import * as functions from "#/modules/config-store/store.functions";
import { onMergeQuery, prPlansQuery, pullRequestQuery } from "#/modules/config-store/store-pull-requests";
import { branchQuery, storeViewPrefix } from "#/modules/config-store/store-view.queries";
import { githubKeys } from "#/modules/github/github.queries";
import { OnMergeButton } from "./OnMerge";

beforeEach(() => {
  vi.stubGlobal("ResizeObserver", class { observe() {} unobserve() {} disconnect() {} });
  Element.prototype.scrollIntoView ??= () => {};
});
afterEach(() => { cleanup(); vi.restoreAllMocks(); vi.unstubAllGlobals(); document.body.replaceChildren(); });

const pr146 = { project: "blog", environment: "pr-146" };
const pullRequest = { repository_id: 7, number: 146 };
const summary = (name: string) => ({ id: `id-${name}`, project: "blog", name, revision: 1 });
const row = (name: string, extra: Partial<MergeRow>): MergeRow => ({
  production: null, lands: null, preview: null, ride: "rides", conflict: false, requires: null,
  row: `w:${name}` as RowId, node: "web", kind: "service", name, ...extra,
});
const rows: MergeRow[] = [
  row("env.API_URL", { production: "http://a", preview: "http://b", lands: "http://b" }),
  row("env.CACHE_TTL", { production: "45", preview: "60", lands: "60", conflict: true }),
  row("env.LOG_LEVEL", { production: "info", preview: "debug", lands: "debug" }),
  row("env.TOKEN", { preview: { secret: true }, lands: { secret: false } }),
  row("env.OLD", { production: "x" }),
  row("env.SAME", { production: "a", preview: "b", lands: "a", ride: "production_keeps" }),
];
const onMerge = (list: MergeRow[] = rows): OnMerge =>
  ({ environment: summary("pr-146"), pull_request: pullRequest, destinations: [{ name: "production", rows: list }] });
const branch: BranchView = {
  environment: summary("pr-146"), parent: "production", kept: false, setup: [], live: [], to_parent: 4,
  closes_at: null, pull_request: pullRequest,
};
const facts = (open: boolean): PullRequestView => ({
  pull_request: {
    repository_id: 7, number: 146, title: "Faster cache", author: "nick", author_id: 7, bot: false, head_repository_id: 7, head_branch: "cache", head: "abc",
    target_branch: "main", commits: 1, open, merge_commit: null, target_head: null, updated: "2026-10-11T00:00:00Z",
  },
  environments: [],
  check: { conclusion: "neutral", title: "5 config changes · TOKEN not set", summary: "", merged_into: null },
});

const plans: PrPlansView = {
  project: { id: "blog", name: "blog" },
  plans: [{
    repository: "acme/blog", repository_id: 7, installation_id: 1, enabled: true, start_from: "production", copy: [], setup: [],
    remove_on_close: true, include_bots: false, open: [],
  }],
};

function open({ at = "/cloud/acme/blog/pr-146", list = onMerge(), isOpen = true } = {}) {
  const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false, staleTime: Infinity } } });
  vi.spyOn(scopes, "useCollectionScope").mockReturnValue({ queryClient, sessionId: "session", userId: "user" });
  vi.spyOn(toast, "error").mockImplementation(() => "toast");
  // The On merge list as the Store has it now: a test changes it to what a write leaves.
  const store = { onMerge: list };
  const views = (query: ConfigQuery): ConfigView | null => {
    switch (query.query) {
      case "branch": return { view: "branch", ...branch };
      case "on_merge": return { view: "on_merge", ...store.onMerge };
      case "pull_request": return { view: "pull_request", ...facts(isOpen) };
      case "pr_plans": return { view: "pr_plans", ...plans };
      default: return null;
    }
  };
  queryClient.setQueryData(githubKeys.web(), { origin: "http://127.0.0.1:4555" });
  for (const query of [branchQuery(pr146), onMergeQuery(pr146), pullRequestQuery(pullRequest), prPlansQuery("blog")] as ConfigQuery[]) {
    queryClient.setQueryData([...storeViewPrefix("acme"), "session", "user", query], { ok: true, value: views(query) });
  }
  vi.spyOn(functions, "readStoreViewServerFn").mockImplementation(({ data }) => {
    const value = views(data.query);
    return Promise.resolve((value ? { ok: true, value } : { ok: false, refusal: { code: "not_found", message: "No", details: {} } }) as never);
  });
  const write = vi.spyOn(functions, "writeStoreServerFn").mockImplementation(() =>
    Promise.resolve({ ok: true, value: { written: "on_merge", ...store.onMerge } } as never));
  const commands = () => write.mock.calls.map(([call]): ConfigCommand | undefined => call?.data.command);

  const root = createRootRoute({ component: Outlet });
  const protectedRoute = createRoute({ getParentRoute: () => root, id: "_protected", component: Outlet });
  const organization = createRoute({ getParentRoute: () => protectedRoute, path: "cloud/$organizationSlug", component: Outlet });
  const projectGroup = createRoute({ getParentRoute: () => organization, id: "_project", component: Outlet });
  const environment = createRoute({
    getParentRoute: () => projectGroup, path: "$projectSlug/$environmentSlug",
    loader: ({ params }) => ({ store: { project: params.projectSlug, environment: params.environmentSlug } }),
    component: () => <><OnMergeButton /><Outlet /></>,
  });
  const canvas = createRoute({ getParentRoute: () => environment, path: "/", component: () => null });
  const router = createRouter({
    routeTree: root.addChildren([protectedRoute.addChildren([organization.addChildren([projectGroup.addChildren([environment.addChildren([canvas])])])])]),
    history: createMemoryHistory({ initialEntries: [at] }),
  });
  render(<QueryClientProvider client={queryClient}><RouterProvider router={router} /></QueryClientProvider>);
  return { store, commands, write };
}

async function panel() {
  fireEvent.click(await screen.findByRole("button", { name: "On merge · 5" }));
  return within(await screen.findByRole("dialog"));
}

function item(list: ReturnType<typeof within>, key: string) {
  const entry = list.getByText(key).closest("li");
  if (entry === null) throw new Error(`No row for ${key}`);
  return entry;
}

async function choose(list: ReturnType<typeof within>, key: string, action: string) {
  fireEvent.click(list.getByRole("button", { name: `More for ${key}` }));
  fireEvent.click(await screen.findByRole("menuitem", { name: action }));
}

it("lists what the merge lands into production, with the check's word", async () => {
  open();
  const list = await panel();
  expect(list.getByText("#146 → production")).toBeTruthy();
  expect(list.getAllByRole("listitem").map((entry) => entry.textContent)).toEqual([
    "webAPI_URLhttp://a→http://b",
    "webCACHE_TTLproduction45→60",
    "webLOG_LEVELinfo→debug",
    "webTOKENSealedNot set in production",
    "webOLDx→Removed",
  ]);
  expect(list.getByText("5 config changes · TOKEN not set")).toBeTruthy();
  expect(list.getByRole("link", { name: /GitHub/u }).getAttribute("href")).toBe("http://127.0.0.1:4555/acme/blog/pull/146/checks");
});

it("opens from the check's link", async () => {
  open({ at: "/cloud/acme/blog/pr-146?panel=on-merge" });
  expect(await screen.findByText("#146 → production")).toBeTruthy();
});

it("shows nothing once the pull request is closed", async () => {
  open({ isOpen: false });
  await waitFor(() => expect(screen.queryByRole("button", { name: /On merge/u })).toBeNull());
});

it("edits what lands, and resets it", async () => {
  const { store, commands } = open();
  const list = await panel();
  await choose(list, "API_URL", "Edit");
  const edit = within(await screen.findByRole("dialog", { name: /API_URL/u }));
  expect(edit.getByText("http://b")).toBeTruthy();
  const input = edit.getByLabelText<HTMLInputElement>("API_URL in production");
  expect(input.value).toBe("http://b");
  store.onMerge = onMerge(rows.map((entry) => entry.name === "env.API_URL" ? { ...entry, ride: "edited", lands: "http://c" } : entry));
  fireEvent.change(input, { target: { value: "http://c" } });
  fireEvent.click(edit.getByRole("button", { name: "Save" }));
  await waitFor(() => expect(item(list, "API_URL").textContent).toBe("webAPI_URLhttp://a→http://cEditedpr-146: http://b"));
  expect(commands()[0]).toEqual({
    command: "set_merge_value", environment: pr146, into: { project: "blog", environment: "production" }, row: "w:env.API_URL", value: "http://c",
  });

  store.onMerge = onMerge();
  await choose(list, "API_URL", "Reset");
  await waitFor(() => expect(item(list, "API_URL").textContent).toBe("webAPI_URLhttp://a→http://b"));
  expect(commands()[1]).toEqual({
    command: "set_merge_value", environment: pr146, into: { project: "blog", environment: "production" }, row: "w:env.API_URL", value: undefined,
  });
});

it("shows an Exclude and an Edit at once, while the Store answers", async () => {
  const { write } = open();
  write.mockReturnValue(new Promise(() => {}));
  const list = await panel();
  await choose(list, "LOG_LEVEL", "Exclude");
  await waitFor(() => expect(item(list, "LOG_LEVEL").textContent).toBe("webLOG_LEVELinfo→debugExcludedInclude"));

  await choose(list, "API_URL", "Edit");
  const edit = within(await screen.findByRole("dialog", { name: /API_URL/u }));
  fireEvent.change(edit.getByLabelText("API_URL in production"), { target: { value: "http://c" } });
  fireEvent.click(edit.getByRole("button", { name: "Save" }));
  await waitFor(() => expect(item(list, "API_URL").textContent).toBe("webAPI_URLhttp://a→http://cEditedpr-146: http://b"));
});

it("offers only Exclude on a removed row", async () => {
  open();
  const list = await panel();
  fireEvent.click(list.getByRole("button", { name: "More for OLD" }));
  expect((await screen.findAllByRole("menuitem")).map((entry) => entry.textContent)).toEqual(["Exclude"]);
});

it("reads a secret the preview removed as removed", async () => {
  open({ list: onMerge([...rows, row("env.KEY", { production: { secret: true } })]) });
  fireEvent.click(await screen.findByRole("button", { name: "On merge · 6" }));
  const list = within(await screen.findByRole("dialog"));
  expect(item(list, "KEY").textContent).toBe("webKEYSealed••••••→Removed");
});

it("reads a Config file row as its path, changed", async () => {
  open({
    list: onMerge([
      ...rows,
      row("files.config.yml", {
        node: "configs.sentry", kind: "config", production: { content: "level: info\n" }, preview: { content: "level: debug\n" },
        lands: { content: "level: debug\n" },
      }),
    ]),
  });
  fireEvent.click(await screen.findByRole("button", { name: "On merge · 6" }));
  const list = within(await screen.findByRole("dialog"));
  expect(item(list, "config.yml").textContent).toBe("configs.sentryconfig.ymlChanged");
});

it("includes a row excluded with its new Service by including the Service", async () => {
  const worker = { node: "worker", kind: "service", requires: "w:worker" as RowId } as const;
  const { commands } = open({
    list: onMerge([
      ...rows,
      row("worker", { ...worker, name: null, requires: null, preview: {}, lands: {}, ride: "excluded" }),
      row("env.QUEUE", { ...worker, preview: "jobs", lands: "jobs", ride: "excluded" }),
    ]),
  });
  const list = await panel();
  fireEvent.click(within(item(list, "QUEUE")).getByRole("button", { name: "Include" }));
  await waitFor(() => expect(commands()).toHaveLength(1));
  expect(commands()[0]).toEqual({ command: "never_sync", environment: pr146, rows: ["w:worker"], off: true });
});

it("shows the Service and its row included at once when the row's Include includes the Service", async () => {
  const worker = { node: "worker", kind: "service", requires: "w:worker" as RowId } as const;
  const { write } = open({
    list: onMerge([
      ...rows,
      row("worker", { ...worker, name: null, requires: null, preview: {}, lands: {}, ride: "excluded" }),
      row("env.QUEUE", { ...worker, preview: "jobs", lands: "jobs", ride: "excluded" }),
    ]),
  });
  write.mockReturnValue(new Promise(() => {}));
  const list = await panel();
  expect(list.getAllByText("Excluded")).toHaveLength(2);
  fireEvent.click(within(item(list, "QUEUE")).getByRole("button", { name: "Include" }));
  await waitFor(() => expect(list.queryAllByText("Excluded")).toHaveLength(0));
});

it("excludes a row, greyed, and includes it again", async () => {
  const { store, commands } = open();
  const list = await panel();
  store.onMerge = onMerge(rows.map((entry) => entry.name === "env.LOG_LEVEL" ? { ...entry, ride: "excluded" } : entry));
  await choose(list, "LOG_LEVEL", "Exclude");
  await waitFor(() => expect(within(item(list, "LOG_LEVEL")).getByText("Excluded")).toBeTruthy());
  expect(commands()[0]).toEqual({ command: "never_sync", environment: pr146, rows: ["w:env.LOG_LEVEL"], off: false });
  expect(screen.getByRole("button", { name: "On merge · 4" })).toBeTruthy();

  store.onMerge = onMerge();
  fireEvent.click(within(item(list, "LOG_LEVEL")).getByRole("button", { name: "Include" }));
  await waitFor(() => expect(within(item(list, "LOG_LEVEL")).queryByText("Excluded")).toBeNull());
  expect(commands()[1]).toEqual({ command: "never_sync", environment: pr146, rows: ["w:env.LOG_LEVEL"], off: true });
});

it("reads a removed node as what the merge does to it, without its record", async () => {
  const removal = (node: string, extra: Partial<MergeRow>): MergeRow =>
    ({ ...row("", {}), row: `${node}:node` as RowId, node, name: null, production: { image: "postgres:17" }, ...extra });
  const users = [{ row: "w:variables.DB" as RowId, node: "worker", kind: "service", name: "env.DB" } as const];
  open({
    list: onMerge([
      ...rows,
      removal("db", { conflict: true }),
      removal("redis", { refused_by: users }),
      removal("cron", { ride: "excluded" }),
      removal("volumes.data", { kind: "volume", ride: "kept", lands: { size: "10GB" }, production: { size: "10GB" } }),
    ]),
  });
  fireEvent.click(await screen.findByRole("button", { name: "On merge · 6" }));
  const list = within(await screen.findByRole("dialog"));
  expect(list.getAllByRole("listitem").slice(rows.length - 1).map((entry) => entry.textContent)).toEqual([
    "dbRemoves dbChanged there",
    "redisIn useworker.env.DB",
    "cronExcludedInclude",
    "volumes.dataKept",
  ]);
  expect(list.queryByRole("button", { name: "More for volumes.data" })).toBeNull();
  fireEvent.click(list.getByRole("button", { name: "More for db" }));
  expect((await screen.findAllByRole("menuitem")).map((entry) => entry.textContent)).toEqual(["Exclude"]);
});

it("reads a Kept row of any kind as Kept, with nothing to do", async () => {
  open({
    list: onMerge([
      ...rows,
      row("mounts.data", { ride: "kept", production: "/data", preview: "/data", lands: "/data" }),
    ]),
  });
  const list = await panel();
  expect(item(list, "Kept").textContent).toBe("webMount of dataKept");
  expect(list.queryByRole("button", { name: "More for Mount of data" })).toBeNull();
});
