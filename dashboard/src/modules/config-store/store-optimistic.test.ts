import { QueryClient } from "@tanstack/react-query";
import type { DeploymentView, DiffView, DomainsView, EnvironmentView, ServiceId, ServicesView, SyncView } from "@ployz/sdk";
import { asTestDouble } from "#/lib/test-double";
import { expect, it } from "vitest";
import { applyOptimistic } from "./store-optimistic";
import { diffQuery, environmentSettingsQuery, servicesQuery, storeViewOptions, syncQuery } from "./store-view.queries";

const ref = { project: "shop", environment: "production" };
// SAFETY: test ids stand in for the Store's UUIDs.
const id = (value: string) => value as ServiceId;
const environment = { id: "env", project: "shop", name: "production", revision: 3 };

function cached() {
  const queryClient = new QueryClient();
  const scope = { queryClient, sessionId: "s", userId: "u" };
  const key = (query: Parameters<typeof storeViewOptions>[2]) => storeViewOptions("acme", scope, query).queryKey;
  const put = (query: Parameters<typeof storeViewOptions>[2], value: DiffView | EnvironmentView | ServicesView) =>
    queryClient.setQueryData<unknown>(key(query), { ok: true, value });
  put(diffQuery(ref), {
    environment, version: "3:1:1", saved: 1, published: false, hints: [], incoming: [], follow_hints: [], total_count: 3, changes: [
      { type: "service", id: "w", name: "web", lifecycle: "update", comparison: null, data: null, settings: [
        { path: "web.replicas", kind: "update", before: 1, after: 3, canRestore: true },
        { path: "web.startCommand", kind: "update", before: null, after: "serve", canRestore: true },
      ] },
      { type: "service", id: "c", name: "cache", lifecycle: "create", comparison: null, data: null, settings: [] },
    ],
  } satisfies DiffView);
  put(environmentSettingsQuery(ref), { environment, settings: [
    { path: "web.replicas", value: 3, default: 1, apply: "staged" },
    { path: "web.startCommand", value: "serve", default: null, apply: "staged" },
  ] } satisfies EnvironmentView);
  put(servicesQuery(ref), { environment, services: [
    { id: id("w"), name: "web", private_dns: "web", source: "image", change: "update", template: null },
    { id: id("c"), name: "cache", private_dns: "cache", source: "image", change: "create", template: null },
  ] } satisfies ServicesView);
  const read = <V,>(query: Parameters<typeof storeViewOptions>[2]) => (queryClient.getQueryData<{ value: V }>(key(query)))?.value;
  return { queryClient, read };
}

it("takes a discarded Setting out of the review at once; what the Store restores comes with its answer", async () => {
  const { queryClient, read } = cached();
  await applyOptimistic(queryClient, "acme", { command: "discard", environment: ref, path: "web.replicas", version: null });
  expect(read<DiffView>(diffQuery(ref))?.changes[0]?.settings.map((row) => row.path)).toEqual(["web.startCommand"]);
  expect(read<DiffView>(diffQuery(ref))?.total_count).toBe(3);
  expect(read<EnvironmentView>(environmentSettingsQuery(ref))?.settings.map((row) => row.value)).toEqual([3, "serve"]);
});

it("empties the review at once on a whole discard; the Services come with its answer", async () => {
  const { queryClient, read } = cached();
  await applyOptimistic(queryClient, "acme", { command: "discard", environment: ref, path: null, version: null });
  expect(read<DiffView>(diffQuery(ref))).toMatchObject({ changes: [], total_count: 0 });
  expect(read<ServicesView>(servicesQuery(ref))?.services.map((service) => service.change)).toEqual(["update", "create"]);
});

it("renames a Service everywhere its name keys a view, and marks a removed one", async () => {
  const { queryClient, read } = cached();
  await applyOptimistic(queryClient, "acme", { command: "rename_service", environment: ref, service: "web", name: "site" });
  expect(read<EnvironmentView>(environmentSettingsQuery(ref))?.settings.map((row) => row.path)).toEqual(["site.replicas", "site.startCommand"]);
  await applyOptimistic(queryClient, "acme", { command: "remove_service", environment: ref, service: "site" });
  await applyOptimistic(queryClient, "acme", { command: "remove_service", environment: ref, service: "cache" });
  expect(read<ServicesView>(servicesQuery(ref))?.services).toMatchObject([{ name: "site", change: "delete" }]);
});

it("guesses a generated domain from the Service's Private DNS, which a rename keeps", async () => {
  const { queryClient, read } = cached();
  const domains = { query: "domains", environment: ref, service: null } as const;
  const scope = { queryClient, sessionId: "s", userId: "u" };
  queryClient.setQueryData<unknown>(storeViewOptions("acme", scope, domains).queryKey, { ok: true, value: { environment, domains: [] } });
  await applyOptimistic(queryClient, "acme", { command: "rename_service", environment: ref, service: "web", name: "site" });
  await applyOptimistic(queryClient, "acme", { command: "add_domain", environment: ref, service: "site", hostname: null, port: null });
  expect(read<DomainsView>(domains)?.domains).toMatchObject([{ kind: "generated", prefix: "web", service: "site" }]);
});

it("shows a Deployment in flight cancelling at once, and leaves one that ended", async () => {
  const queryClient = new QueryClient();
  const scope = { queryClient, sessionId: "s", userId: "u" };
  const view = (id: string, status: DeploymentView["status"]) => {
    const query = { query: "deployment", id } as const;
    const value = asTestDouble<DeploymentView>()({ id, status, in_flight: status === "queued" || status === "running" });
    queryClient.setQueryData<unknown>(storeViewOptions("acme", scope, query).queryKey, { ok: true, value });
    return () => queryClient.getQueryData<{ value: DeploymentView }>(storeViewOptions("acme", scope, query).queryKey)?.value.status;
  };
  const queued = view("q", "queued");
  const running = view("r", "running");
  const ended = view("a", "applied");
  for (const deployment of ["q", "r", "a"]) await applyOptimistic(queryClient, "acme", { command: "cancel", deployment });
  expect([queued(), running(), ended()]).toEqual(["cancelling", "cancelling", "applied"]);
});

it("shows a generated domain's new prefix at once, pink, under the same Cluster Domain", async () => {
  const { queryClient, read } = cached();
  const domains = { query: "domains", environment: ref, service: null } as const;
  const scope = { queryClient, sessionId: "s", userId: "u" };
  queryClient.setQueryData<unknown>(storeViewOptions("acme", scope, domains).queryKey, { ok: true, value: { environment, domains: [
    { kind: "generated", prefix: "web", hostname: "web.acme.ployz.app", service: "web", port: null, status: "ready", reason: null, action: null },
  ] } });
  await applyOptimistic(queryClient, "acme", { command: "set_generated_domain", environment: ref, service: "web", prefix: "shop" });
  expect(read<DomainsView>(domains)?.domains).toMatchObject([{ prefix: "shop", hostname: "shop.acme.ployz.app" }]);
  expect(read<DiffView>(diffQuery(ref))?.changes[0]?.settings.map((row) => row.path)).toContain("web.managedHostnames");
});

it("renames a Project in the Projects list at once", async () => {
  const queryClient = new QueryClient();
  const scope = { queryClient, sessionId: "s", userId: "u" };
  const key = storeViewOptions("acme", scope, { query: "projects" }).queryKey;
  queryClient.setQueryData<unknown>(key, { ok: true, value: { projects: [{ id: "p", name: "shop", default_environment: "production", environments: [] }] } });
  await applyOptimistic(queryClient, "acme", { command: "rename_project", project: "shop", name: "store" });
  expect(queryClient.getQueryData<{ value: { projects: { name: string }[] } }>(key)?.value.projects[0]?.name).toBe("store");
});

it("shows each command of a Batch at once, as it would alone", async () => {
  const { queryClient, read } = cached();
  await applyOptimistic(queryClient, "acme", { command: "batch", environment: ref, commands: [
    { command: "create_service", id: "p", environment: ref, name: "postgres", image: "postgres:17" },
    { command: "edit", environment: ref, expect: null, changes: [{ op: "set", path: "postgres.replicas", value: 1 }] },
  ] });
  expect(read<ServicesView>(servicesQuery(ref))?.services.map((service) => [service.name, service.change]))
    .toEqual([["web", "update"], ["cache", "create"], ["postgres", "create"]]);
});

it("shows a variable marked Never sync at once, and synced again", async () => {
  const { queryClient, read } = cached();
  const marks = () => read<EnvironmentView>(environmentSettingsQuery(ref))?.never_synced;
  await applyOptimistic(queryClient, "acme", { command: "never_sync", environment: ref, paths: ["web.env.B", "web.env.A"] });
  expect(marks()).toEqual(["web.env.A", "web.env.B"]);
  await applyOptimistic(queryClient, "acme", { command: "never_sync", environment: ref, paths: ["web.env.A"], off: true });
  expect(marks()).toEqual(["web.env.B"]);
});

it("moves a change marked Never sync into the Sync dialog's never-synced list at once", async () => {
  const { queryClient } = cached();
  const query = syncQuery({ project: "shop", environment: "fix-web" });
  const key = storeViewOptions("acme", { queryClient, sessionId: "s", userId: "u" }, query).queryKey;
  const side = (name: string) => ({ id: name, project: "shop", name, revision: 1 });
  const change = (variable: string) => ({ key: `w:variables.${variable}`, node: "web", label: `web.env.${variable}`, from: "a", into: "b",
    ticked: true, changed: false, new: false, secret: false });
  queryClient.setQueryData<unknown>(key, { ok: true, value: {
    from: side("fix-web"), into: side("production"), version: "1:1", rows: [change("A"), change("B")],
    never_synced: [{ key: "w:variables.C", node: "web", label: "web.env.C", marked_in: ["fix-web", "production"] }],
  } satisfies SyncView });
  const read = () => queryClient.getQueryData<{ value: SyncView }>(key)?.value;
  const fixWeb = { project: "shop", environment: "fix-web" };

  await applyOptimistic(queryClient, "acme", { command: "never_sync", environment: fixWeb, paths: ["web.env.A"] });
  expect(read()?.rows.map(({ label }) => label)).toEqual(["web.env.B"]);
  expect(read()?.never_synced.map(({ label, marked_in }) => [label, marked_in])).toEqual([
    ["web.env.C", ["fix-web", "production"]], ["web.env.A", ["fix-web"]],
  ]);
  // Synced again where it's marked: gone from the list once no side marks it; the row comes back with the refetch.
  await applyOptimistic(queryClient, "acme", { command: "never_sync", environment: fixWeb, paths: ["web.env.A", "web.env.C"], off: true });
  expect(read()?.never_synced.map(({ label, marked_in }) => [label, marked_in])).toEqual([["web.env.C", ["production"]]]);
  // An Environment on neither side changes nothing.
  await applyOptimistic(queryClient, "acme", { command: "never_sync", environment: { project: "shop", environment: "staging" }, paths: ["web.env.B"] });
  expect(read()?.rows.map(({ label }) => label)).toEqual(["web.env.B"]);
});
