import { QueryClient } from "@tanstack/react-query";
import type { ConfigItemView, ConfigsView, DeploymentView, DiffView, DomainsView, EnvironmentView, MergeRow, OnMerge, ServiceListing, ServicesView, SyncView } from "@ployz/sdk";
import { asTestDouble } from "#/lib/test-double";
import { rowId, serviceId } from "#/test/ids";
import { describe, expect, it } from "vitest";
import { applyOptimistic } from "./store-optimistic";
import { onMergeQuery } from "./store-pull-requests";
import { diffQuery, environmentSettingsQuery, servicesQuery, storeViewOptions, syncQuery } from "./store-view.queries";

const ref = { project: "shop", environment: "production" };
const environment = { id: "env", project: "shop", name: "production", revision: 3 };

function cached() {
  const queryClient = new QueryClient();
  const scope = { queryClient, sessionId: "s", userId: "u" };
  const key = (query: Parameters<typeof storeViewOptions>[2]) => storeViewOptions("acme", scope, query).queryKey;
  const put = (query: Parameters<typeof storeViewOptions>[2], value: DiffView | EnvironmentView | ServicesView) =>
    queryClient.setQueryData<unknown>(key(query), { ok: true, value: { view: query.query, ...value } });
  put(diffQuery(ref), {
    environment, version: "3:1:1", saved: 1, published: false, incoming: [], follow_hints: [], included: [], total_count: 3, changes: [
      { type: "service", id: "w", row: rowId("w:node"), name: "web", lifecycle: "update", comparison: null, data: null, restarts: [], settings: [
        { path: "web.replicas", kind: "update", before: 1, after: 3, canRestore: true, row: null },
        { path: "web.startCommand", kind: "update", before: null, after: "serve", canRestore: true, row: null },
      ] },
      { type: "service", id: "c", row: rowId("c:node"), name: "cache", lifecycle: "create", comparison: null, data: null, restarts: [], settings: [] },
    ],
  } satisfies DiffView);
  put(environmentSettingsQuery(ref), { environment, settings: [
    { path: "web.replicas", value: 3, default: 1, apply: "staged" },
    { path: "web.startCommand", value: "serve", default: null, apply: "staged" },
  ] } satisfies EnvironmentView);
  put(servicesQuery(ref), { environment, services: [
    { id: serviceId("w"), row: rowId("w:node"), name: "web", private_dns: "web", source: "image", change: "update", template: null },
    { id: serviceId("c"), row: rowId("c:node"), name: "cache", private_dns: "cache", source: "image", change: "create", template: null },
  ] } satisfies ServicesView);
  const read = <V,>(query: Parameters<typeof storeViewOptions>[2]) => (queryClient.getQueryData<{ value: V }>(key(query)))?.value;
  return { queryClient, read };
}

it("keeps the current review until Discard answers with its authoritative inverse", async () => {
  const { queryClient, read } = cached();
  await applyOptimistic(queryClient, "acme", { command: "discard", environment: ref, path: "web.replicas", version: null });
  expect(read<DiffView>(diffQuery(ref))?.changes[0]?.settings.map((row) => row.path)).toEqual(["web.replicas", "web.startCommand"]);
  expect(read<DiffView>(diffQuery(ref))?.total_count).toBe(3);
  expect(read<EnvironmentView>(environmentSettingsQuery(ref))?.settings.map((row) => row.value)).toEqual([3, "serve"]);
});

it("keeps review and Services while whole Discard is pending", async () => {
  const { queryClient, read } = cached();
  await applyOptimistic(queryClient, "acme", { command: "discard", environment: ref, path: null, version: null });
  expect(read<DiffView>(diffQuery(ref))?.total_count).toBe(3);
  expect(read<ServicesView>(servicesQuery(ref))?.services.map((service) => service.change)).toEqual(["update", "create"]);
});

it("keeps exact Config files and affected Services visible until Discard answers", async () => {
  const { queryClient, read } = cached();
  const key = storeViewOptions("acme", { queryClient, sessionId: "s", userId: "u" }, diffQuery(ref)).queryKey;
  queryClient.setQueryData<unknown>(key, { ok: true, value: {
    ...read<DiffView>(diffQuery(ref)), changes: [{
      type: "config", id: "s", row: rowId("s:node"), name: "sentry", lifecycle: "update", comparison: null, data: null, restarts: ["web"],
      settings: ["app.conf", "app.conf.bak"].map((name) => ({
        path: `configs.sentry.files.${name}`, kind: "update", before: null, after: null, canRestore: true, row: null,
      })),
    }],
  } });
  await applyOptimistic(queryClient, "acme", { command: "discard", environment: ref, path: "configs.sentry.files.app.conf", version: null });
  expect(read<DiffView>(diffQuery(ref))?.changes[0]?.settings.map((row) => row.path)).toEqual(["configs.sentry.files.app.conf", "configs.sentry.files.app.conf.bak"]);
  expect(read<DiffView>(diffQuery(ref))?.changes[0]?.restarts).toEqual(["web"]);
  await applyOptimistic(queryClient, "acme", { command: "discard", environment: ref, path: "configs.sentry", version: null });
  expect(read<DiffView>(diffQuery(ref))?.total_count).toBe(3);
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

it("adds another generated domain by its prefix and changes only the one named", async () => {
  const { queryClient, read } = cached();
  const domains = { query: "domains", environment: ref, service: null } as const;
  const scope = { queryClient, sessionId: "s", userId: "u" };
  queryClient.setQueryData<unknown>(storeViewOptions("acme", scope, domains).queryKey, { ok: true, value: { environment, domains: [
    { kind: "generated", prefix: "web", hostname: "web.acme.ployz.app", service: "web", port: null, status: "ready", reason: null, action: null },
  ] } });
  const prefixes = () => read<DomainsView>(domains)?.domains.map((domain) => domain.kind === "generated" ? domain.prefix : domain.hostname);
  // Without a prefix the one it has stays alone.
  await applyOptimistic(queryClient, "acme", { command: "add_domain", environment: ref, service: "web", hostname: null, port: null });
  expect(prefixes()).toEqual(["web"]);
  await applyOptimistic(queryClient, "acme", { command: "add_domain", environment: ref, service: "web", hostname: null, prefix: "shop", port: 3000 });
  expect(read<DomainsView>(domains)?.domains).toMatchObject([{ prefix: "web", port: null }, { prefix: "shop", port: 3000 }]);
  // With two, an unnamed change waits for the Store; a named one moves only that one.
  await applyOptimistic(queryClient, "acme", { command: "set_generated_domain", environment: ref, service: "web", prefix: "api" });
  expect(prefixes()).toEqual(["web", "shop"]);
  await applyOptimistic(queryClient, "acme", { command: "set_generated_domain", environment: ref, service: "web", domain: "web.acme.ployz.app", prefix: "www" });
  expect(read<DomainsView>(domains)?.domains).toMatchObject([{ prefix: "www", hostname: "www.acme.ployz.app" }, { prefix: "shop" }]);
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
  const row = (name: string) => rowId(`w:variables.${name}`);
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
  const db: ServiceListing = { id: serviceId("d2"), row: rowId("d:node"), name: "db", private_dns: "db", source: "image", change: null, template: null };
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
    row: rowId(`a:variables.${name}`), node: "api", kind: "service" as const, name: `env.${name}`,
    change: "changed" as const, from: "x", into: "y", ticked: true, requires: null, secret: null, held_by: null,
  });
  queryClient.setQueryData<unknown>(storeViewOptions("acme", { queryClient, sessionId: "s", userId: "u" }, query).queryKey, { ok: true, value: {
    from: { ...environment, name: "fix-api" }, into: environment, version: "1", rows: [offer("A"), offer("B")], never_synced: [], proposal: null,
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
    row: rowId(`a:variables.${name}`), node: "api", kind: "service" as const, name: `env.${name}`,
    marks: environments.map((environment) => ({ environment, row: rowId(`a:variables.${name}`) })),
  });
  queryClient.setQueryData<unknown>(storeViewOptions("acme", { queryClient, sessionId: "s", userId: "u" }, query).queryKey, { ok: true, value: {
    from: { ...environment, name: "fix-api" }, into: environment, version: "1", rows: [], proposal: null,
    never_synced: [marked("A", ["fix-api"]), marked("B", ["fix-api", "production"]), marked("C", ["fix-api"])],
  } satisfies SyncView });
  await applyOptimistic(queryClient, "acme", { command: "never_sync", environment: fix, rows: [marked("A", []).row, marked("B", []).row], off: true });
  expect(read<SyncView>(query)?.never_synced).toEqual([marked("B", ["production"]), marked("C", ["fix-api"])]);
});


describe("the On merge list", () => {
  const pr = { project: "shop", environment: "pr-146" };
  const merging = (name: string, production: MergeRow["production"], preview: MergeRow["preview"]): MergeRow => ({
    row: rowId(`w:variables.${name}`), node: "web", kind: "service", name: `env.${name}`, production, preview,
    lands: preview, ride: "rides", conflict: false, requires: null,
  });
  function listed() {
    const queryClient = new QueryClient();
    const key = storeViewOptions("acme", { queryClient, sessionId: "s", userId: "u" }, onMergeQuery(pr)).queryKey;
    queryClient.setQueryData<unknown>(key, { ok: true, value: {
      environment: { ...environment, name: "pr-146" }, pull_request: { repository_id: 1, number: 146 },
      destinations: [{ name: "production", rows: [merging("CACHE_TTL", "30", "60"), merging("TOKEN", null, { secret: true })] }],
    } satisfies OnMerge });
    const rows = () => queryClient.getQueryData<{ value: OnMerge }>(key)?.value.destinations[0]?.rows
      .map(({ name, ride, lands }) => ({ name, ride, lands }));
    return { queryClient, rows };
  }

  it("excludes a row at once, landing nothing, and includes it again as the PR Environment has it", async () => {
    const { queryClient, rows } = listed();
    const cache = "w:variables.CACHE_TTL";
    await applyOptimistic(queryClient, "acme", { command: "never_sync", environment: pr, rows: [cache] });
    expect(rows()?.[0]).toEqual({ name: "env.CACHE_TTL", ride: "excluded", lands: "60" });
    await applyOptimistic(queryClient, "acme", { command: "never_sync", environment: pr, rows: [cache], off: true });
    expect(rows()?.[0]).toEqual({ name: "env.CACHE_TTL", ride: "rides", lands: "60" });
  });

  it("never moves a Kept Volume, and a removal excluded or included again still lands nothing", async () => {
    const queryClient = new QueryClient();
    const key = storeViewOptions("acme", { queryClient, sessionId: "s", userId: "u" }, onMergeQuery(pr)).queryKey;
    const node = (name: string, ride: MergeRow["ride"], lands: MergeRow["lands"]): MergeRow => ({
      row: rowId(`${name}:node`), node: name, kind: name.startsWith("volumes.") ? "volume" : "service", name: null,
      production: { image: "x" }, preview: null, lands, ride, conflict: false, requires: null,
    });
    queryClient.setQueryData<unknown>(key, { ok: true, value: {
      environment: { ...environment, name: "pr-146" }, pull_request: { repository_id: 1, number: 146 },
      destinations: [{ name: "production", rows: [node("db", "rides", null), node("volumes.data", "kept", { size: "10GB" })] }],
    } satisfies OnMerge });
    const rows = () => queryClient.getQueryData<{ value: OnMerge }>(key)?.value.destinations[0]?.rows
      .map(({ node: at, ride, lands }) => [at, ride, lands]);
    const both = ["db:node", "volumes.data:node"];
    await applyOptimistic(queryClient, "acme", { command: "never_sync", environment: pr, rows: both });
    expect(rows()).toEqual([["db", "excluded", null], ["volumes.data", "kept", { size: "10GB" }]]);
    await applyOptimistic(queryClient, "acme", { command: "never_sync", environment: pr, rows: both, off: true });
    expect(rows()).toEqual([["db", "rides", null], ["volumes.data", "kept", { size: "10GB" }]]);
  });

  it("lands an Edit's value at once, a secret's sealed, and taken back lands the PR Environment's", async () => {
    const { queryClient, rows } = listed();
    const into = { project: "shop", environment: "production" };
    await applyOptimistic(queryClient, "acme", { command: "set_merge_value", environment: pr, into, row: "w:variables.CACHE_TTL", value: "45" });
    await applyOptimistic(queryClient, "acme", { command: "set_merge_value", environment: pr, into, row: "w:variables.TOKEN", value: "tok" });
    expect(rows()).toEqual([
      { name: "env.CACHE_TTL", ride: "edited", lands: "45" },
      { name: "env.TOKEN", ride: "edited", lands: { secret: true } },
    ]);
    await applyOptimistic(queryClient, "acme", { command: "set_merge_value", environment: pr, into, row: "w:variables.CACHE_TTL" });
    await applyOptimistic(queryClient, "acme", { command: "set_merge_value", environment: pr, into, row: "w:variables.TOKEN" });
    // Production lacks the secret: it lands Not set.
    expect(rows()).toEqual([
      { name: "env.CACHE_TTL", ride: "rides", lands: "60" },
      { name: "env.TOKEN", ride: "rides", lands: { secret: false } },
    ]);
  });
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
  queryClient.setQueryData<unknown>(key(configs), { ok: true, value: { view: "configs", environment, configs: [config("sentry"), config("other")] } });
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


it.each(["delete", "rename"])("keeps an inactive Config cache isolated after %s and same-name replacement", async (action) => {
  const queryClient = new QueryClient();
  const scope = { queryClient, sessionId: "s", userId: "u" };
  const key = (query: Parameters<typeof storeViewOptions>[2]) => storeViewOptions("acme", scope, query).queryKey;
  const old: ConfigItemView = { environment, id: "old", lineage: "old", name: "sentry", deployed: true, change: null, mounts: [],
    files: [{ name: "app.conf", bytes: 3, mode: "0444", uid: 0, gid: 0, references: [] }], contents: { "app.conf": "old" } };
  const replacement: ConfigItemView = { ...old, id: "new", lineage: "new", deployed: false, change: "create", contents: { "app.conf": "new" } };
  const listing = { query: "configs", environment: ref } as const;
  const oldQuery = { query: "config", environment: ref, config: "@old" } as const;
  const newQuery = { query: "config", environment: ref, config: "@new" } as const;
  for (const [query, value] of [[listing, { view: "configs", environment, configs: [old] }], [oldQuery, old], [newQuery, replacement]] as const) {
    queryClient.setQueryData<unknown>(key(query), { ok: true, value });
  }
  if (action === "delete") {
    await applyOptimistic(queryClient, "acme", { command: "delete_config", environment: ref, config: "sentry" });
  } else {
    await applyOptimistic(queryClient, "acme", { command: "rename_config", environment: ref, config: "sentry", name: "renamed" });
    // The committed listing refreshes; the inactive exact item remains cached.
    queryClient.setQueryData<unknown>(key(listing), { ok: true, value: { view: "configs", environment, configs: [{ ...old, name: "renamed" }] } });
  }
  await applyOptimistic(queryClient, "acme", { command: "create_config", environment: ref, id: "new", name: "sentry", mounts: [] });
  const read = (query: typeof oldQuery | typeof newQuery) => queryClient.getQueryData<{ value: ConfigItemView }>(key(query))?.value;
  await applyOptimistic(queryClient, "acme", { command: "put_config_file", environment: ref, config: "sentry", file: "app.conf", content: "replacement" });
  expect(read(oldQuery)).toEqual(old);
  expect(read(newQuery)?.contents).toEqual({ "app.conf": "replacement" });
  await applyOptimistic(queryClient, "acme", { command: "attach_config", environment: ref, config: "sentry", service: "web", dir: "/etc/app" });
  expect(read(oldQuery)).toEqual(old);
  expect(read(newQuery)?.mounts).toEqual([{ service: "web", dir: "/etc/app" }]);
  await applyOptimistic(queryClient, "acme", { command: "detach_config", environment: ref, config: "sentry", service: "web" });
  expect(read(oldQuery)).toEqual(old);
  expect(read(newQuery)?.mounts).toEqual([]);
  await applyOptimistic(queryClient, "acme", { command: "remove_config_file", environment: ref, config: "sentry", file: "app.conf" });
  expect(read(oldQuery)).toEqual(old);
  expect(read(newQuery)?.contents).toEqual({});
  expect(queryClient.getQueryData<{ value: ConfigsView }>(key(listing))?.value.configs[0]).toMatchObject({ id: "old", contents: old.contents });
});

it("waits for the Store when no Config listing establishes an optimistic target identity", async () => {
  const queryClient = new QueryClient();
  const key = storeViewOptions("acme", { queryClient, sessionId: "s", userId: "u" }, { query: "config", environment: ref, config: "@old" }).queryKey;
  const old: ConfigItemView = { environment, id: "old", lineage: "old", name: "sentry", deployed: true, change: null, mounts: [], files: [], contents: {} };
  queryClient.setQueryData<unknown>(key, { ok: true, value: old });
  await applyOptimistic(queryClient, "acme", { command: "put_config_file", environment: ref, config: "sentry", file: "app.conf", content: "replacement" });
  expect(queryClient.getQueryData(key)).toEqual({ ok: true, value: old });
});

it("keeps both same-name Config identities visible while Discard is pending", async () => {
  const { queryClient, read } = cached();
  const key = storeViewOptions("acme", { queryClient, sessionId: "s", userId: "u" }, diffQuery(ref)).queryKey;
  const node = (id: string): DiffView["changes"][number] => ({ type: "config", id, row: rowId(`${id}:node`),
    name: "sentry", lifecycle: "update", comparison: null, data: null, restarts: [], settings: [
      { path: `configs.@${id}.files.app.conf`, kind: "update", before: null, after: null, canRestore: true, row: null },
    ] });
  queryClient.setQueryData<unknown>(key, { ok: true, value: { ...read<DiffView>(diffQuery(ref)), changes: [node("old"), node("new")] } });
  await applyOptimistic(queryClient, "acme", { command: "discard", environment: ref, path: "configs.@old", version: null });
  expect(read<DiffView>(diffQuery(ref))?.changes).toEqual([node("old"), node("new")]);
});
