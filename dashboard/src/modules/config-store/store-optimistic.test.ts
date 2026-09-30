import { QueryClient } from "@tanstack/react-query";
import type { DeploymentView, DiffView, DomainsView, EnvironmentView, ServiceId, ServicesView } from "@ployz/sdk";
import { asTestDouble } from "#/lib/test-double";
import { expect, it } from "vitest";
import { applyOptimistic } from "./store-optimistic";
import { diffQuery, environmentSettingsQuery, servicesQuery, storeViewOptions } from "./store-view.queries";

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
    environment, version: "3:1:1", saved: 1, published: false, hints: [], total_count: 3, changes: [
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

it("shows a discarded Setting back at its deployed value, gone from the review", async () => {
  const { queryClient, read } = cached();
  await applyOptimistic(queryClient, "acme", { command: "discard", environment: ref, path: "web.replicas", version: null });
  expect(read<DiffView>(diffQuery(ref))?.changes[0]?.settings.map((row) => row.path)).toEqual(["web.startCommand"]);
  // The count comes with the write's committed review.
  expect(read<DiffView>(diffQuery(ref))?.total_count).toBe(3);
  expect(read<EnvironmentView>(environmentSettingsQuery(ref))?.settings.map((row) => row.value)).toEqual([1, "serve"]);
});

it("drops a discarded new Service, and a whole discard empties the review", async () => {
  const { queryClient, read } = cached();
  await applyOptimistic(queryClient, "acme", { command: "discard", environment: ref, path: null, version: null });
  expect(read<DiffView>(diffQuery(ref))).toMatchObject({ changes: [], total_count: 0 });
  expect(read<ServicesView>(servicesQuery(ref))?.services).toEqual([{ id: "w", name: "web", private_dns: "web", source: "image", change: null }]);
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

it("cancels a queued Deployment at once, and shows a running one cancelling", async () => {
  const queryClient = new QueryClient();
  const scope = { queryClient, sessionId: "s", userId: "u" };
  const view = (id: string, status: DeploymentView["status"]) => {
    const query = { query: "deployment", id } as const;
    queryClient.setQueryData<unknown>(storeViewOptions("acme", scope, query).queryKey, { ok: true, value: asTestDouble<DeploymentView>()({ id, status }) });
    return () => queryClient.getQueryData<{ value: DeploymentView }>(storeViewOptions("acme", scope, query).queryKey)?.value.status;
  };
  const queued = view("q", "queued");
  const running = view("r", "running");
  await applyOptimistic(queryClient, "acme", { command: "cancel", deployment: "q" });
  await applyOptimistic(queryClient, "acme", { command: "cancel", deployment: "r" });
  expect([queued(), running()]).toEqual(["cancelled", "cancelling"]);
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
