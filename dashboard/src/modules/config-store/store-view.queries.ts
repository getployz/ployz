import { infiniteQueryOptions, keepPreviousData, queryOptions, skipToken, useMutationState, useQueries, useQuery, useSuspenseInfiniteQuery, useSuspenseQueries, type Query, type QueryClient } from "@tanstack/react-query";
import type {
  BranchPlanQuery, BranchPreset, BranchQuery, BuildLogQuery, Change, ConfigQuery, ConfigView, DeploymentQuery, DeploymentsQuery,
  DeploymentsView, DiffQuery, DomainsQuery, EnvironmentQuery, EnvironmentRef, EnvironmentsQuery, EnvironmentView, MoveQuery, NamespaceQuery,
  ProjectsQuery, RemovalsQuery, ServicesQuery, VolumesQuery,
} from "@ployz/sdk";
import { Schema } from "effect";
import type { CollectionScope } from "#/collections/scope";
import { useCollectionScope } from "#/collections/use-collection-scope";
import type { StoreViewName } from "#/collections/read.contract";
import { readStoreViewServerFn } from "./store.functions";
import type { CommittedViews, StoreResult, StoreViewOf } from "./store.contract";
import { prPlansQuery, pullRequestQuery } from "./store-pull-requests";

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
  numbered_deployment: ["store_deployment"],
  build_log: ["store_deployment"],
  // Admission fixes an Environment's Namespace.
  namespace: ["store_environment", "store_deployment"],
  namespaces: ["store_environment", "store_deployment"],
  // Whether a domain is deployed follows Deployments; its certificate and DNS are Cloud's observations, which
  // change with no Store write, so a domains view also polls while one is on its way (`DOMAIN_POLL_MS`).
  domains: ["store_environment", "store_deployment"],
  domain: ["store_environment", "store_deployment"],
  // Whether a Volume is deployed, and what a Deploy removes, come from Applied State.
  volumes: ["store_environment", "store_deployment"],
  volume: ["store_environment", "store_deployment"],
  removals: ["store_environment", "store_deployment"],
  // A Branch's Live Nodes and pending Update follow what its Parent and ancestors run.
  branch: ["store_environment", "store_deployment"],
  // A plan reads the Environment's Working State and what it and its ancestors run.
  branch_plan: ["store_environment", "store_deployment"],
  build_order: ["store_organization"],
  // A Move compares a Branch with its Parent's Working and Applied State; a PR's Conditional Save reads its facts.
  move: ["store_environment", "store_deployment", "store_pull_request"],
  // The Project names its Default Environment; a removal is a Deployment.
  environments: ["store_project", "store_environment", "store_deployment"],
  projects: ["store_project", "store_environment"],
  // Plans list the repositories Working States deploy from, and name nodes of the start-from Environment.
  pr_plans: ["store_project", "store_environment"],
  // PR Environments, their Deployments, each Destination's Working State, and the facts GitHub last told Cloud.
  pull_request: ["store_environment", "store_deployment", "store_pull_request"],
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

export function queryOf(query: Query): ConfigQuery | null {
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

/**
 * After a write: refetches the views of the Environment it named (`key`; null, every view of the Organization) and
 * waits for them, so the committed state shows before the overlay goes. The views the write's answer carried are
 * already committed and aren't read again; the Organization's views follow the change stream.
 */
export async function refetchAfterWrite(queryClient: QueryClient, organizationSlug: string, key: string | null, carried: CommittedViews = {}) {
  await queryClient.invalidateQueries({ queryKey: storeViewPrefix(organizationSlug), predicate: (query) => {
    const config = queryOf(query);
    if (config === null || key === null) return config !== null;
    return isOfEnvironment(query, key) && carriedView(config, carried) === undefined;
  } });
}

/** The view a write's answer carried for a cached query of its Environment, if it carried that one. */
function carriedView(config: ConfigQuery, views: CommittedViews) {
  if (config.query === "diff") return views.diff;
  if (config.query === "services") return views.services;
  return config.query === "environment" && config.path === null && config.all ? views.environment : undefined;
}

/**
 * A write's committed views, in every cached view of the same kind and Environment: the review's rows, count and pink
 * arrive with the write, before any refetch. A read already in flight is cancelled first, so it can't land over them.
 */
export async function putCommittedViews(queryClient: QueryClient, organizationSlug: string, key: string, views: CommittedViews) {
  for (const query of queryClient.getQueryCache().findAll({ queryKey: storeViewPrefix(organizationSlug) })) {
    const config = queryOf(query);
    const view = config === null ? undefined : carriedView(config, views);
    if (view === undefined || !isOfEnvironment(query, key)) continue;
    await queryClient.cancelQueries({ queryKey: query.queryKey, exact: true });
    queryClient.setQueryData(query.queryKey, { ok: true, value: view });
  }
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

const isSecret = Schema.is(Schema.Struct({ secret: Schema.Unknown }));

/**
 * Shows edits not yet committed over an Environment view, in order: what the user sees while saves run. It knows no
 * edit rules: the dashboard only sends `set PATH VALUE` and `unset PATH` for rows the view lists (a new variable adds
 * its row), so a pending edit shows as its value, or the row's default once unset. A secret shows as reads show it,
 * `{"secret": true}`. What the Store makes of an edit arrives with the committed view.
 */
export function withPendingChanges(view: EnvironmentView, changes: readonly Change[]): EnvironmentView {
  if (changes.length === 0) return view;
  const settings = view.settings.map((row) => ({ ...row }));
  for (const change of changes) {
    if (change.op === "patch") continue;
    let row = settings.find((candidate) => candidate.path === change.path);
    if (!row && change.op === "set") settings.push(row = { path: change.path, value: null, default: null, apply: "staged" });
    if (!row) continue;
    const value = change.op === "set" ? change.value : row.default;
    row.value = isSecret(value) ? { secret: true } : value;
  }
  return { ...view, settings };
}

/** An Environment's Settings (every one, defaults included): the view Service editors read and edit. */
export function environmentSettingsQuery(environment: EnvironmentRef): { query: "environment" } & EnvironmentQuery {
  return { query: "environment", environment, path: null, all: true };
}

/** An Environment's Services, staged removals included: what its canvas draws. */
export function servicesQuery(environment: EnvironmentRef): { query: "services" } & ServicesQuery {
  return { query: "services", environment };
}

/** The Namespace an Environment runs in: how runtime evidence names its Services (`NAMESPACE/PRIVATE_DNS`). */
export function namespaceQuery(environment: EnvironmentRef): { query: "namespace" } & NamespaceQuery {
  return { query: "namespace", environment };
}

/** What the next Deploy changes in an Environment: the pink trail on its canvas and drawers. */
export function diffQuery(environment: EnvironmentRef): { query: "diff" } & DiffQuery {
  return { query: "diff", environment };
}

/** An Environment's public domains with their status: the Networking section of every Service drawer. */
export function domainsQuery(environment: EnvironmentRef): { query: "domains" } & DomainsQuery {
  return { query: "domains", environment, service: null };
}

/** An Environment's Volumes with where Services mount them: the canvas's Volumes and their links. */
export function volumesQuery(environment: EnvironmentRef): { query: "volumes" } & VolumesQuery {
  return { query: "volumes", environment };
}

/** The Organization's Projects, each with its Default Environment and its Environments' names. */
export function projectsQuery(): { query: "projects" } & ProjectsQuery {
  return { query: "projects" };
}

/** A Project's Environments: which is the Default, each Branch's Parent, and any removal from the Servers. */
export function environmentsQuery(project: string): { query: "environments" } & EnvironmentsQuery {
  return { query: "environments", project };
}

/** The Volumes whose data taking the Environment off the Servers deletes. */
export function removalsQuery(environment: EnvironmentRef): { query: "removals" } & RemovalsQuery {
  return { query: "removals", environment, remove: true };
}

/** One Deployment: its Node Outcomes, builds, recorded Deploy Preview and outcome. */
export function deploymentQuery(id: string): { query: "deployment" } & DeploymentQuery {
  return { query: "deployment", id };
}

/** One Service's build log in a Deployment, by its name when admitted. */
export function buildLogQuery(deployment: string, service: string): { query: "build_log" } & BuildLogQuery {
  return { query: "build_log", deployment, service };
}

/** A Branch: its Parent, what it uses live and what Update would stage. Anything else is refused: not a Branch. */
export function branchQuery(environment: EnvironmentRef): { query: "branch" } & BranchQuery {
  return { query: "branch", environment };
}

/** What Save would put in a Branch's Parent. */
export function saveQuery(branch: EnvironmentRef): { query: "move" } & MoveQuery {
  return { query: "move", move: "save", from: branch };
}

/** What Update would bring into a Branch from what its Parent runs. */
export function updateQuery(branch: EnvironmentRef): { query: "move" } & MoveQuery {
  return { query: "move", move: "update", into: branch };
}

/** What a Branch of `from` would copy and use live, for the picks so far (by name), or for a preset around `focus`. */
export function branchPlanQuery(from: EnvironmentRef, focus: string[], picks: { copy: string[] } | { preset: BranchPreset }):
  { query: "branch_plan" } & BranchPlanQuery {
  return { query: "branch_plan", from, focus, copy: "copy" in picks ? picks.copy : [], preset: "preset" in picks ? picks.preset : null };
}

/**
 * A Branch plan while the user picks (null reads none): each pick reads a new plan, and the last one shows until it
 * arrives. The New branch loader prefetches the first.
 */
export function useBranchPlan(organizationSlug: string, query: ReturnType<typeof branchPlanQuery> | null) {
  const options = storeViewOptions(organizationSlug, useCollectionScope(), query ?? branchPlanQuery({ project: null, environment: null }, [], { copy: [] }));
  return useQuery({ ...options, staleTime: Infinity, queryFn: query ? options.queryFn : skipToken, placeholderData: keepPreviousData }).data;
}

/** The first page of a paged Store view has no cursor. */
const FIRST_PAGE: string | null = null;

const deploymentsPage = (environment: EnvironmentRef, cursor: string | null): { query: "deployments" } & DeploymentsQuery =>
  ({ query: "deployments", environment, limit: null, cursor });

/**
 * An Environment's Deployments, newest first, a Store page at a time: whoever admitted them, CLI or dashboard. Keyed
 * like a Store view (its first page's query), so the change stream and a committed write refetch every loaded page.
 */
export function storeDeploymentsOptions(organizationSlug: string, scope: CollectionScope, environment: EnvironmentRef) {
  return infiniteQueryOptions({
    queryKey: [...storeViewPrefix(organizationSlug), scope.sessionId, scope.userId, deploymentsPage(environment, null)] as const,
    staleTime: Infinity,
    initialPageParam: FIRST_PAGE,
    queryFn: async ({ pageParam, signal }) => {
      const result = await readStoreViewServerFn({ data: { organizationSlug, query: deploymentsPage(environment, pageParam) }, signal });
      // SAFETY: the Store answers a `deployments` query with a `deployments` view.
      return requireView(result) as DeploymentsView;
    },
    getNextPageParam: (page) => page.next_cursor,
  });
}

/** An Environment's Deployments, prefetched by the page's loader (`prefetchStoreDeployments`). */
export function useStoreDeployments(organizationSlug: string, environment: EnvironmentRef) {
  return useSuspenseInfiniteQuery(storeDeploymentsOptions(organizationSlug, useCollectionScope(), environment));
}

/** An Environment's Deployments in flight. In-flight Deployments are the newest, so they sit on the first page. */
export function useInFlightDeployments(organizationSlug: string, environment: EnvironmentRef) {
  return useStoreDeployments(organizationSlug, environment).data.pages[0]?.deployments.filter((deployment) => deployment.in_flight) ?? [];
}

/** A view the page can't show without: a refusal fails the route, whose error component words it. */
export function requireView<T>(result: StoreResult<T>): T {
  if (!result.ok) throw new Error(result.refusal.message);
  return result.value;
}

/** A fresh read of one Store view as a step of a user command (what a removal deletes, before confirming). */
export async function fetchStoreView<Q extends ConfigQuery>(organizationSlug: string, scope: CollectionScope, query: Q) {
  return requireView(await scope.queryClient.fetchQuery({ ...storeViewOptions(organizationSlug, scope, query), staleTime: 0 }));
}

/**
 * Reads Store views together, prefetched by the page's loader (`prefetchStoreViews`). An Environment view shows this
 * tab's pending edits over the committed one; a failed save drops its edit, which is the rollback.
 */
export function useStoreViews<const Qs extends readonly ConfigQuery[]>(organizationSlug: string, queries: Qs):
  { [K in keyof Qs]: StoreResult<StoreViewOf<Qs[K]>> } {
  const scope = useCollectionScope();
  const results = useSuspenseQueries({ queries: queries.map((query) => storeViewOptions(organizationSlug, scope, query)) });
  const pending = useMutationState({
    filters: { mutationKey: ["store-edit", organizationSlug], status: "pending" },
    // SAFETY: store-write.ts files only `Change[]` variables under `storeEditKey`, whose last part is the Environment.
    select: (mutation) => ({ key: mutation.options.mutationKey?.[2], changes: mutation.state.variables as Change[] }),
  });
  // SAFETY: each result answers the query at its index with the view of the same name.
  return results.map(({ data: result }, index) => {
    const query = queries[index];
    if (!result.ok || result.value.view !== "environment" || !query || !("environment" in query)) return result;
    const key = environmentKey(query.environment);
    const changes = pending.flatMap((edit) => edit.key === key ? edit.changes : []);
    return changes.length === 0 ? result : { ok: true, value: withPendingChanges(result.value, changes) };
  }) as { [K in keyof Qs]: StoreResult<StoreViewOf<Qs[K]>> };
}

/** Reads one Store view; see `useStoreViews`. */
export function useStoreView<Q extends ConfigQuery>(organizationSlug: string, query: Q): StoreResult<StoreViewOf<Q>> {
  return useStoreViews(organizationSlug, [query] as const)[0];
}

/**
 * Changes open pull requests saved into `environment` for their merge (standing Conditional Saves), by pull request:
 * the bottom bar's "goes live when #N merges". Chrome, so nothing waits on it; the Project's plans name the open ones.
 */
// ponytail: one pull request view per open PR of the Project; a Store view of saves into an Environment when PRs pile up.
export function useSavesInto(organizationSlug: string, project: string, environment: string) {
  const scope = useCollectionScope();
  const plans = useCachedStoreView(organizationSlug, prPlansQuery(project));
  const open = plans?.ok ? plans.value.plans.flatMap((plan) => plan.open.map((pr) => ({ repository_id: plan.repository_id, number: pr.number }))) : [];
  const views = useQueries({ queries: open.map((pr) => storeViewOptions(organizationSlug, scope, pullRequestQuery(pr))) });
  return views.flatMap(({ data }) => {
    if (!data?.ok || !data.value.pull_request) return [];
    const { number } = data.value.pull_request;
    return data.value.environments.flatMap((pr) => pr.destinations.flatMap((destination) =>
      destination.name === environment && destination.save?.standing
        ? [{ number, changes: destination.save.changes, environment: pr.environment.name }] : []));
  });
}

/**
 * A Store view for chrome that must not wait on it (null reads nothing): the cached answer, if any, kept fresh like any
 * other. The canvas lights an open Deployment Page's nodes from the view that page's loader prefetched.
 */
export function useCachedStoreView<Q extends ConfigQuery>(organizationSlug: string, query: Q | null): StoreResult<StoreViewOf<Q>> | undefined {
  const scope = useCollectionScope();
  const queries: ReturnType<typeof storeViewOptions<Q>>[] = query ? [storeViewOptions(organizationSlug, scope, query)] : [];
  return useQueries({ queries })[0]?.data;
}
