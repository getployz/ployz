import { queryOptions, useMutationState, useSuspenseQuery, type Query, type QueryClient } from "@tanstack/react-query";
import type { Change, ConfigQuery, ConfigView, DiffQuery, DomainsQuery, EnvironmentQuery, EnvironmentRef, EnvironmentView, JsonValue, ServicesQuery } from "@ployz/sdk";
import { Option, Schema } from "effect";
import type { CollectionScope } from "#/collections/scope";
import { useCollectionScope } from "#/collections/use-collection-scope";
import type { StoreViewName } from "#/collections/read.contract";
import { readStoreViewServerFn } from "./store.functions";
import type { StoreResult, StoreViewOf } from "./store.contract";

/**
 * The Store tables' change names that refresh each query kind. A new Query kind must say which tables back it,
 * or the Organization change stream never refreshes its views.
 */
const refreshedBy = {
  environment: ["store_environment"],
  diff: ["store_environment", "store_deployment"],
  plan: ["store_environment", "store_deployment"],
  // A Service's lifecycle comes from the review, which compares against what's deployed.
  service: ["store_environment", "store_deployment"],
  services: ["store_environment", "store_deployment"],
  deployments: ["store_deployment"],
  deployment: ["store_deployment"],
  // Admission fixes an Environment's Namespace.
  namespace: ["store_environment", "store_deployment"],
  // Whether a domain is deployed follows Deployments; its certificate and DNS are Cloud's observations, which
  // change with no Store write, so a domains view also polls while one is on its way (`DOMAIN_POLL_MS`).
  domains: ["store_environment", "store_deployment"],
  domain: ["store_environment", "store_deployment"],
} satisfies Record<ConfigQuery["query"], readonly StoreViewName[]>;

/** How often a domains view rereads while a domain is still setting up or waits on the user's DNS. */
const DOMAIN_POLL_MS = 30_000;

/** A domains view with a domain that time, Ployz or the user's DNS will move on: certificates, DNS, the Cluster Domain. */
function settling(result: StoreResult<ConfigView> | undefined) {
  return result?.ok === true && result.value.view === "domains"
    && result.value.domains.some((domain) => domain.status !== "ready" && domain.action?.type !== "deploy");
}

export const storeViewPrefix = (organizationSlug: string) => ["store-view", organizationSlug] as const;

/**
 * One bounded Store view: an Environment's Settings, its diff or plan, one page of its Deployments, one Deployment.
 * A refusal (`not_found`, `ambiguous`) is the view's answer, not a failed read. The Organization change stream
 * refetches it when a table behind it changes; only a settling domains view polls.
 */
export function storeViewOptions<Q extends ConfigQuery>(organizationSlug: string, scope: CollectionScope, query: Q) {
  const queryFn = async ({ signal }: { signal: AbortSignal }) =>
    // SAFETY: the Store answers each query kind with the view of the same name.
    await readStoreViewServerFn({ data: { organizationSlug, query }, signal }) as StoreResult<StoreViewOf<Q>>;
  return queryOptions({
    queryKey: [...storeViewPrefix(organizationSlug), scope.sessionId, scope.userId, query] as const,
    staleTime: Infinity,
    refetchInterval: (cached) => settling(cached.state.data) ? DOMAIN_POLL_MS : false,
    queryFn,
  });
}

function queryOf(query: Query): ConfigQuery | null {
  const [prefix, , , , config] = query.queryKey;
  // SAFETY: every `store-view` key is built by storeViewOptions, with its ConfigQuery last.
  return prefix === "store-view" ? config as ConfigQuery : null;
}

/** The change stream named a Store table family: refetch every view of the Organization it backs. */
export function refetchStoreViews(organizationSlug: string, scope: CollectionScope, name: StoreViewName) {
  void scope.queryClient.invalidateQueries({
    queryKey: storeViewPrefix(organizationSlug),
    predicate: (query) => {
      const config = queryOf(query);
      if (config === null) return false;
      const families: readonly StoreViewName[] = refreshedBy[config.query];
      return families.includes(name);
    },
  });
}

/** Names an Environment the same way however a ref spells it; the Store resolves omitted names to defaults. */
export function environmentKey(ref: EnvironmentRef) {
  return `${ref.project ?? ""}/${ref.environment ?? ""}`;
}

function isOfEnvironment(query: Query, key: string) {
  const config = queryOf(query);
  return config !== null && "environment" in config && environmentKey(config.environment) === key;
}

/** Refetches every view of one Environment and waits for them, so a committed write shows before its overlay goes. */
export async function refetchEnvironmentViews(queryClient: QueryClient, organizationSlug: string, key: string) {
  await queryClient.invalidateQueries({ queryKey: storeViewPrefix(organizationSlug), predicate: (query) => isOfEnvironment(query, key) });
}

/** The newest Working State revision any cached view of the Environment shows. */
export function cachedRevision(queryClient: QueryClient, organizationSlug: string, key: string) {
  let latest: number | null = null;
  for (const query of queryClient.getQueryCache().findAll({ queryKey: storeViewPrefix(organizationSlug) })) {
    if (!isOfEnvironment(query, key)) continue;
    // SAFETY: store-view queries hold StoreResult views; an Environment's views carry its summary, if any.
    const result = query.state.data as StoreResult<{ environment?: { revision: number } }> | undefined;
    const revision = result?.ok ? result.value.environment?.revision : undefined;
    if (revision !== undefined && (latest === null || revision > latest)) latest = revision;
  }
  return latest;
}

/** The mutation key of an Environment's pending edits; `store-write.ts` files them under it. */
export const storeEditKey = (organizationSlug: string, key: string) => ["store-edit", organizationSlug, key] as const;

/** A patch's value: Settings by name. The Store refuses any other shape. */
const decodePatch = Schema.decodeUnknownOption(Schema.Record(Schema.String, Schema.MutableJson));

const isObject = (value: JsonValue): value is { [key: string]: JsonValue } =>
  typeof value === "object" && value !== null && !Array.isArray(value);

/** A value as reads show it: a secret (a variable's, a registry credential) is `{"secret": true}`, never its plaintext. */
function shown(value: JsonValue): JsonValue {
  return isObject(value) && "secret" in value ? { secret: true } : value;
}

/** A variable's path, `SERVICE.env.KEY`: a set may add it, and `{"value", "exported"}` writes both of its rows. */
const VARIABLE_PATH = /^[^.]+\.env\.[^.]+$/u;

/** Applies edits not yet committed over an Environment view, in order: what the user sees while saves run. */
export function withPendingChanges(view: EnvironmentView, changes: readonly Change[]): EnvironmentView {
  if (changes.length === 0) return view;
  const settings = view.settings.map((row) => ({ ...row }));
  const values = view.values ? { ...view.values } : view.values;
  const assign = (path: string, value: (row: EnvironmentView["settings"][number]) => JsonValue) => {
    const row = settings.find((candidate) => candidate.path === path);
    if (!row) return;
    row.value = shown(value(row));
    // `values` exists only on a one-Service view, so the row found is that Service's.
    const setting = path.split(".")[1];
    if (values && setting && setting !== "env") {
      if (row.value === null) delete values[setting];
      else values[setting] = row.value;
    }
  };
  const setVariable = (path: string, value: JsonValue) => {
    const write = isObject(value) && !("secret" in value) ? value : { value };
    for (const [at, next, fallback] of [[path, write["value"], null], [`${path}.exported`, write["exported"], false]] as const) {
      if (next === undefined) continue;
      if (!settings.some((row) => row.path === at)) settings.push({ path: at, value: fallback, default: fallback, apply: "staged" });
      assign(at, () => next);
    }
  };
  for (const change of changes) {
    if (change.op === "set" && VARIABLE_PATH.test(change.path)) setVariable(change.path, change.value);
    else if (change.op === "set") assign(change.path, () => change.value);
    else if (change.op === "unset") assign(change.path, (row) => row.default);
    else {
      const patch = decodePatch(change.value);
      for (const [setting, value] of Option.isSome(patch) ? Object.entries(patch.value) : []) assign(`${change.path}.${setting}`, () => value);
    }
  }
  return { ...view, settings, values };
}

/** An Environment's Settings (every one, defaults included): the view Service editors read and edit. */
export function environmentSettingsQuery(environment: EnvironmentRef): { query: "environment" } & EnvironmentQuery {
  return { query: "environment", environment, path: null, all: true };
}

/** An Environment's Services, staged removals included: what its canvas draws. */
export function servicesQuery(environment: EnvironmentRef): { query: "services" } & ServicesQuery {
  return { query: "services", environment };
}

/** What the next Deploy changes in an Environment: the pink trail on its canvas and drawers. */
export function diffQuery(environment: EnvironmentRef): { query: "diff" } & DiffQuery {
  return { query: "diff", environment };
}

/** An Environment's public domains with their status: the Networking section of every Service drawer. */
export function domainsQuery(environment: EnvironmentRef): { query: "domains" } & DomainsQuery {
  return { query: "domains", environment, service: null };
}

/** A view the page can't show without: a refusal fails the route, whose error component words it. */
export function requireView<T>(result: StoreResult<T>): T {
  if (!result.ok) throw new Error(result.refusal.message);
  return result.value;
}

/**
 * Reads one Store view, prefetched by the page's loader (`prefetchStoreViews`). An Environment view shows this tab's
 * pending edits over the committed one; a failed save drops its edit, which is the rollback.
 */
export function useStoreView<Q extends ConfigQuery>(organizationSlug: string, query: Q): StoreResult<StoreViewOf<Q>> {
  const result = useSuspenseQuery(storeViewOptions(organizationSlug, useCollectionScope(), query)).data;
  const key = "environment" in query ? environmentKey(query.environment) : "";
  const pending = useMutationState({
    filters: { mutationKey: storeEditKey(organizationSlug, key), status: "pending" },
    // SAFETY: store-write.ts files only `Change[]` variables under this key.
    select: (mutation) => mutation.state.variables as Change[],
  });
  if (!result.ok || result.value.view !== "environment" || pending.length === 0) return result;
  // SAFETY: an `environment` view answers an `environment` query.
  return { ok: true, value: withPendingChanges(result.value as EnvironmentView, pending.flat()) as StoreViewOf<Q> };
}
