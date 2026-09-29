import { QueryClient } from "@tanstack/react-query";
import type { DiffView, EnvironmentView, ServiceId, ServicesView } from "@ployz/sdk";
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
    { id: id("w"), name: "web", private_dns: "web", source: "image", change: "update" },
    { id: id("c"), name: "cache", private_dns: "cache", source: "image", change: "create" },
  ] } satisfies ServicesView);
  const read = <V,>(query: Parameters<typeof storeViewOptions>[2]) => (queryClient.getQueryData<{ value: V }>(key(query)))?.value;
  return { queryClient, read };
}

it("shows a discarded Setting back at its deployed value, gone from the review", () => {
  const { queryClient, read } = cached();
  applyOptimistic(queryClient, "acme", { command: "discard", environment: ref, path: "web.replicas", version: null });
  expect(read<DiffView>(diffQuery(ref))?.changes[0]?.settings.map((row) => row.path)).toEqual(["web.startCommand"]);
  expect(read<DiffView>(diffQuery(ref))?.total_count).toBe(2);
  expect(read<EnvironmentView>(environmentSettingsQuery(ref))?.settings.map((row) => row.value)).toEqual([1, "serve"]);
});

it("drops a discarded new Service, and a whole discard empties the review", () => {
  const { queryClient, read } = cached();
  applyOptimistic(queryClient, "acme", { command: "discard", environment: ref, path: null, version: null });
  expect(read<DiffView>(diffQuery(ref))).toMatchObject({ changes: [], total_count: 0 });
  expect(read<ServicesView>(servicesQuery(ref))?.services).toEqual([{ id: "w", name: "web", private_dns: "web", source: "image", change: null }]);
});

it("renames a Service everywhere its name keys a view, and marks a removed one", () => {
  const { queryClient, read } = cached();
  applyOptimistic(queryClient, "acme", { command: "rename_service", environment: ref, service: "web", name: "site" });
  expect(read<EnvironmentView>(environmentSettingsQuery(ref))?.settings.map((row) => row.path)).toEqual(["site.replicas", "site.startCommand"]);
  applyOptimistic(queryClient, "acme", { command: "remove_service", environment: ref, service: "site" });
  applyOptimistic(queryClient, "acme", { command: "remove_service", environment: ref, service: "cache" });
  expect(read<ServicesView>(servicesQuery(ref))?.services).toMatchObject([{ name: "site", change: "delete" }]);
});
