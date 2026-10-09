import { QueryClient } from "@tanstack/react-query";
import type { ConfigItemView, ConfigsView, DeploymentView, DiffView, DomainsView, EnvironmentView, RowId, ServiceId, ServiceListing, ServicesView, SyncView } from "@ployz/sdk";
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
      { type: "service", id: "w", row: "w:node" as RowId, name: "web", lifecycle: "update", comparison: null, data: null, restarts: [], settings: [
        { path: "web.replicas", kind: "update", before: 1, after: 3, canRestore: true, row: null },
        { path: "web.startCommand", kind: "update", before: null, after: "serve", canRestore: true, row: null },
      ] },
      { type: "service", id: "c", row: "c:node" as RowId, name: "cache", lifecycle: "create", comparison: null, data: null, restarts: [], settings: [] },
    ],
  } satisfies DiffView);
  put(environmentSettingsQuery(ref), { environment, settings: [
    { path: "web.replicas", value: 3, default: 1, apply: "staged" },
    { path: "web.startCommand", value: "serve", default: null, apply: "staged" },
  ] } satisfies EnvironmentView);
  put(servicesQuery(ref), { environment, services: [
    { id: id("w"), row: "w:node" as RowId, name: "web", private_dns: "web", source: "image", change: "update", template: null },
    { id: id("c"), row: "c:node" as RowId, name: "cache", private_dns: "cache", source: "image", change: "create", template: null },
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

it("shows a mark and an unmark at once, by the rows they name", async () => {
  const { queryClient, read } = cached();
  const row = (name: string) => `w:variables.${name}` as RowId;
  queryClient.setQueryData<{ ok: true; value: EnvironmentView }>(
    storeViewOptions("acme", { queryClient, sessionId: "s", userId: "u" }, environmentSettingsQuery(ref)).queryKey,
    (cached) => cached && { ok: true, value: { ...cached.value, never_synced: [row("A"), row("B")] } },
  );
  const marks = () => read<EnvironmentView>(environmentSettingsQuery(ref))?.never_synced;
  await applyOptimistic(queryClient, "acme", { command: "never_sync", environment: ref, rows: [row("C")] });
  expect(marks()).toEqual([row("A"), row("B"), row("C")]);
  await applyOptimistic(queryClient, "acme", { command: "never_sync", environment: ref, rows: [row("A")], off: true });
  expect(marks()).toEqual([row("B"), row("C")]);
});

it("stages an edit of a Service the review lacks under its listing's row", async () => {
  const { queryClient, read } = cached();
  // A Branch's copy: its own id, its lineage's row.
  const db: ServiceListing = { id: id("d2"), row: "d:node" as RowId, name: "db", private_dns: "db", source: "image", change: null, template: null };
  queryClient.setQueryData<{ ok: true; value: ServicesView }>(
    storeViewOptions("acme", { queryClient, sessionId: "s", userId: "u" }, servicesQuery(ref)).queryKey,
    (cached) => cached && { ok: true, value: { ...cached.value, services: [...cached.value.services, db] } },
  );
  await applyOptimistic(queryClient, "acme", { command: "add_domain", environment: ref, service: "db", port: 5432, hostname: "db.example.com" });
  expect(read<DiffView>(diffQuery(ref))?.changes.at(-1)).toMatchObject({ name: "db", id: "d2", row: "d:node" });
});

it("takes a newly marked row out of a Sync from the marking Environment at once, marked by it", async () => {
  const { queryClient, read } = cached();
  const fix = { project: "shop", environment: "fix-api" };
  const query = syncQuery(fix, "production");
  const offer = (name: string) => ({
    row: `a:variables.${name}` as RowId, node: "api", kind: "service" as const, name: `env.${name}`,
    change: "changed" as const, from: "x", into: "y", ticked: true, requires: null, secret: null,
  });
  queryClient.setQueryData<unknown>(storeViewOptions("acme", { queryClient, sessionId: "s", userId: "u" }, query).queryKey, { ok: true, value: {
    from: { ...environment, name: "fix-api" }, into: environment, at_merge: null, version: "1", rows: [offer("A"), offer("B")], never_synced: [],
  } satisfies SyncView });
  await applyOptimistic(queryClient, "acme", { command: "never_sync", environment: fix, rows: [offer("A").row] });
  const view = read<SyncView>(query);
  expect(view?.rows.map((row) => row.name)).toEqual(["env.B"]);
  expect(view?.never_synced).toEqual([{ row: "a:variables.A", node: "api", kind: "service", name: "env.A", marks: [{ environment: "fix-api", row: "a:variables.A" }] }]);
});

it("takes an unmarked row's mark out of a Sync at once, dropping a row left with none", async () => {
  const { queryClient, read } = cached();
  const fix = { project: "shop", environment: "fix-api" };
  const query = syncQuery(fix, "production");
  const marked = (name: string, environments: string[]) => ({
    row: `a:variables.${name}` as RowId, node: "api", kind: "service" as const, name: `env.${name}`,
    marks: environments.map((environment) => ({ environment, row: `a:variables.${name}` as RowId })),
  });
  queryClient.setQueryData<unknown>(storeViewOptions("acme", { queryClient, sessionId: "s", userId: "u" }, query).queryKey, { ok: true, value: {
    from: { ...environment, name: "fix-api" }, into: environment, at_merge: null, version: "1", rows: [],
    never_synced: [marked("A", ["fix-api"]), marked("B", ["fix-api", "production"]), marked("C", ["fix-api"])],
  } satisfies SyncView });
  await applyOptimistic(queryClient, "acme", { command: "never_sync", environment: fix, rows: [marked("A", []).row, marked("B", []).row], off: true });
  expect(read<SyncView>(query)?.never_synced).toEqual([marked("B", ["production"]), marked("C", ["fix-api"])]);
});


it("removes only the named Config file from both the listing and open item", async () => {
  const queryClient = new QueryClient();
  const scope = { queryClient, sessionId: "s", userId: "u" };
  const config = (name: string): ConfigItemView => ({
    environment, id: name, lineage: name, name, mounts: [], deployed: false, change: "create",
    files: ["config.yml", "conf.d/site.yml"].map((file) => ({ name: file, bytes: 1, mode: "0444", uid: 0, gid: 0, references: [] })),
    contents: { "config.yml": "a", "conf.d/site.yml": "b" },
  });
  const configs = { query: "configs", environment: ref } as const;
  const item = { query: "config", environment: ref, config: "sentry" } as const;
  const other = { query: "config", environment: ref, config: "other" } as const;
  const key = (query: Parameters<typeof storeViewOptions>[2]) => storeViewOptions("acme", scope, query).queryKey;
  queryClient.setQueryData<unknown>(key(configs), { ok: true, value: { environment, configs: [config("sentry"), config("other")] } });
  queryClient.setQueryData<unknown>(key(item), { ok: true, value: config("sentry") });
  queryClient.setQueryData<unknown>(key(other), { ok: true, value: config("other") });
  await applyOptimistic(queryClient, "acme", { command: "remove_config_file", environment: ref, config: "sentry", file: "conf.d/site.yml" });
  expect(queryClient.getQueryData<{ value: ConfigsView }>(key(configs))?.value.configs.map((one) => one.files.map((file) => file.name)))
    .toEqual([["config.yml"], ["config.yml", "conf.d/site.yml"]]);
  const shown = queryClient.getQueryData<{ value: ConfigItemView }>(key(item))?.value;
  expect(shown?.files.map((file) => file.name)).toEqual(["config.yml"]);
  expect(shown?.contents).toEqual({ "config.yml": "a" });
  expect(queryClient.getQueryData<{ value: ConfigItemView }>(key(other))?.value).toEqual(config("other"));
});
