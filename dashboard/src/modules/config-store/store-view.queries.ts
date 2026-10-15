import { infiniteQueryOptions, keepPreviousData, queryOptions, skipToken, useQueries, useQuery, useSuspenseInfiniteQuery, useSuspenseQueries, type Query, type QueryClient } from "@tanstack/react-query";
import type {
  BranchPlanQuery, BranchPreset, BranchQuery, BuildLogQuery, ConfigItemQuery, ConfigQuery, ConfigsQuery, ConfigView, DeploymentQuery, DeploymentsQuery,
  DiffQuery, DomainsQuery, EnvironmentQuery, EnvironmentRef, EnvironmentsQuery, NamespaceQuery,
  ProjectsQuery, RemovalsQuery, ServicesQuery, SyncQuery, VolumesQuery,
} from "@ployz/sdk";
import { Schema } from "effect";
import type { CollectionScope } from "#/collections/scope";
import { useCollectionScope } from "#/collections/use-collection-scope";
import type { StoreViewName } from "#/collections/read.contract";
import { readStoreViewServerFn } from "./store.functions";
import { MAX_SHOWN_VIEWS, StoreQuery, type StoreResult, StoreView, type StoreViewOf, type ViewAnswer, viewOf } from "./store.contract";
import { usePendingMarks, showPending } from "./store-pending";

/**
 * The Store tables' change names that refresh each query kind. A new Query kind must say which tables back it,
 * or the Organization change stream never refreshes its views.
 */
const refreshedBy = {
  environment: ["store_environment"],
  diff: ["store_environment", "store_deployment"],
  history: ["store_environment"],
  history_preview: ["store_environment", "store_deployment"],
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
  configs: ["store_environment", "store_deployment"],
  config: ["store_environment", "store_deployment"],
  removals: ["store_environment", "store_deployment"],
  removed: ["store_environment", "store_deployment"],
  // A Branch's Live Nodes follow what its Parent and ancestors run; a PR Environment counts toward its pull
  // request's Destination.
  branch: ["store_environment", "store_deployment", "store_pull_request"],
  // A plan reads the Environment's Working State and what it and its ancestors run.
  branch_plan: ["store_environment", "store_deployment"],
  build_order: ["store_organization"],
  // A Sync compares two Environments over what they last shared.
  sync: ["store_environment", "store_deployment"],
  // The Project names its Default Environment; a removal is a Deployment.
  environments: ["store_project", "store_environment", "store_deployment"],
  projects: ["store_project", "store_environment"],
  // Removals the Store still owes: Environments marked for removal and their Deployments.
  due: ["store_environment", "store_deployment"],
  consented: ["store_environment", "store_deployment"],
  // Plans list the repositories Working States deploy from, and name nodes of the start-from Environment.
  pr_plans: ["store_project", "store_environment"],
  // PR Environments, their Deployments, each Destination's Working State, and the facts GitHub last told Cloud.
  pull_request: ["store_environment", "store_deployment", "store_pull_request"],
  // What a merge lands: both Environments' Working and Saved, the Edits, and the pull request's facts.
  on_merge: ["store_environment", "store_pull_request"],
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
 * A refusal (`not_found`, `ambiguous`) is the view's answer, not a failed read; a busy Store (`unavailable`) is a
 * failed read. The Organization change stream refetches it when a table behind it changes; only a settling domains
 * view polls.
 */
export function storeViewOptions<Q extends ConfigQuery>(organizationSlug: string, scope: CollectionScope, query: Q) {
  const queryFn = async ({ signal }: { signal: AbortSignal }) => {
    const result = await readView(organizationSlug, query, signal);
    // A busy Store is a failed read, retried; the view keeps its last answer.
    if (!result.ok && result.refusal.code === "unavailable") throw new Error(result.refusal.message);
    return result;
  };
  return queryOptions({
    queryKey: [...storeViewPrefix(organizationSlug), scope.sessionId, scope.userId, query] as const,
    staleTime: Infinity,
    refetchInterval: (cached) => settling(cached.state.data) ? DOMAIN_POLL_MS : false,
    structuralSharing: keepNewer,
    queryFn,
  });
}

/** Reads `query`'s view: the Store answers each query kind with the view of the same name. */
async function readView<Q extends ConfigQuery>(organizationSlug: string, query: Q, signal: AbortSignal): Promise<StoreResult<StoreViewOf<Q>>> {
  const result = await readStoreViewServerFn({ data: { organizationSlug, query }, signal });
  if (!result.ok) return result;
  const view = viewOf<Q["query"]>(result.value, query);
  if (view === undefined) throw new Error(`The Store answered a ${query.query} query with a ${result.value.view} view.`);
  return { ok: true, value: view };
}

const isStoreQuery = Schema.is(StoreQuery);

export function queryOf(query: Query): ConfigQuery | null {
  const [prefix, , , , config] = query.queryKey;
  return prefix === "store-view" && isStoreQuery(config) ? config : null;
}

const isAnswered = Schema.is(Schema.Struct({ ok: Schema.Literal(true), value: StoreView }));

/** The `kind` view a cached Store view query holds, if it answered with one. */
export function cachedView<K extends ConfigQuery["query"]>(query: Query, kind: K) {
  const result = query.state.data;
  return isAnswered(result) ? viewOf(result.value, { query: kind }) : undefined;
}

/** Views with one more read queued behind the read in flight. */
const rereads = new WeakSet<Query>();

/**
 * The change stream named Store table families: refetch every view of the Organization they back. A read in flight
 * finishes, and one more read follows it, since it may predate the change. Restarting it instead never finishes
 * while changes come faster than it reads, and the Store still does each abandoned read.
 */
export function refetchStoreViews(organizationSlug: string, scope: CollectionScope, names: readonly StoreViewName[]) {
  const filters = {
    queryKey: storeViewPrefix(organizationSlug),
    predicate: (query: Query) => {
      const config = queryOf(query);
      return config !== null && refreshedBy[config.query].some((name) => names.includes(name));
    },
  };
  for (const query of scope.queryClient.getQueryCache().findAll({ ...filters, fetchStatus: "fetching" })) {
    const reading = query.promise;
    if (reading === undefined || rereads.has(query)) continue;
    rereads.add(query);
    void reading.catch(() => undefined).finally(() => {
      rereads.delete(query);
      void scope.queryClient.invalidateQueries({ queryKey: query.queryKey, exact: true }, { cancelRefetch: false });
    });
  }
  void scope.queryClient.invalidateQueries(filters, { cancelRefetch: false });
}

/** Names an Environment the same way however a ref spells it; the Store resolves omitted names to defaults. */
export function environmentKey(ref: EnvironmentRef) {
  return `${ref.project ?? ""}/${ref.environment ?? ""}`;
}

/** Whether a cached view reads the Environment: its own views, and a Sync's from either side. */
function isOfEnvironment(query: Query, key: string) {
  const config = queryOf(query);
  if (config?.query === "sync") return [config.from, config.into].some((side) => side && environmentKey(side) === key);
  return config !== null && "environment" in config && environmentKey(config.environment) === key;
}

/**
 * The views this tab shows of a write's Environments, newest read first (cap `MAX_SHOWN_VIEWS`): the write's answer
 * carries each, read after it committed. Only views with an observer are shown; the rest are invalidated.
 */
export function shownQueries(queryClient: QueryClient, organizationSlug: string, keys: ReadonlySet<string> | null) {
  return viewsOf(queryClient, organizationSlug, keys)
    .filter((query) => query.getObserversCount() > 0 && !isPaged(query))
    .sort((a, b) => b.state.dataUpdatedAt - a.state.dataUpdatedAt)
    .flatMap((query) => queryOf(query) ?? [])
    .slice(0, MAX_SHOWN_VIEWS);
}

function viewsOf(queryClient: QueryClient, organizationSlug: string, keys: ReadonlySet<string> | null) {
  return queryClient.getQueryCache().findAll({ queryKey: storeViewPrefix(organizationSlug) })
    .filter((query) => queryOf(query) !== null && (keys === null || [...keys].some((key) => isOfEnvironment(query, key))));
}

/** A paged list (Deployments) caches pages, not one view: a write's answer can't replace it, so it is reread. */
const isPaged = (query: Query) => carriesPages(query.state.data);

const carriesPages = Schema.is(Schema.Struct({ pages: Schema.Array(Schema.Unknown) }));

/**
 * A write's answers, in every cached view of the same query, through `keepNewer`: a read already in flight is
 * cancelled first, so it can't land over them. Views of the write's Environments it didn't answer, or answered with a
 * refusal, are reread.
 */
export async function putAnswers(queryClient: QueryClient, organizationSlug: string, keys: ReadonlySet<string> | null, answers: readonly ViewAnswer[]) {
  const answered = new Set<Query>();
  for (const query of viewsOf(queryClient, organizationSlug, keys)) {
    const config = queryOf(query);
    const answer = answers.find((candidate) => JSON.stringify(candidate.query) === JSON.stringify(config));
    if (answer === undefined || !answer.result.ok) continue;
    answered.add(query);
    await queryClient.cancelQueries({ queryKey: query.queryKey, exact: true });
    queryClient.setQueryData(query.queryKey, answer.result);
  }
  await invalidateViews(queryClient, organizationSlug, keys, answered);
}

/** Rereads the views of `keys` (null, every view of the Organization), except `skip`. */
export function invalidateViews(queryClient: QueryClient, organizationSlug: string, keys: ReadonlySet<string> | null, skip: ReadonlySet<Query> = new Set()) {
  const leftovers = new Set(viewsOf(queryClient, organizationSlug, keys).filter((query) => !skip.has(query)));
  return queryClient.invalidateQueries({ queryKey: storeViewPrefix(organizationSlug), predicate: (query) => leftovers.has(query) });
}

/**
 * The one revision guard: a read or a write's answer never replaces a view with an older one of the same Environment.
 * Working State's revision orders views, then a diff's Saved revision, which a publish moves alone. Nothing else is
 * ordered: a view of another Environment under the name (one deleted and recreated) replaces, and so does a Deploy's
 * progress, which no revision counts.
 */
function keepNewer<T>(old: T | undefined, next: T): T {
  if (!carriesRevision(old) || !carriesRevision(next) || old.value.environment.id !== next.value.environment.id) return next;
  const working = next.value.environment.revision - old.value.environment.revision;
  return working < 0 || (working === 0 && (next.value.saved ?? 0) < (old.value.saved ?? 0)) ? old : next;
}

const carriesRevision = Schema.is(Schema.Struct({
  ok: Schema.Literal(true),
  value: Schema.Struct({
    environment: Schema.Struct({ id: Schema.String, revision: Schema.Number }),
    saved: Schema.optional(Schema.NullOr(Schema.Number)),
  }),
}));

/** The newest Working State revision any cached view of the Environment shows. */
export function cachedRevision(queryClient: QueryClient, organizationSlug: string, key: string) {
  let latest: number | null = null;
  for (const query of queryClient.getQueryCache().findAll({ queryKey: storeViewPrefix(organizationSlug) })) {
    if (!isOfEnvironment(query, key)) continue;
    const result = query.state.data;
    const revision = carriesRevision(result) ? result.value.environment.revision : undefined;
    if (revision !== undefined && (latest === null || revision > latest)) latest = revision;
  }
  return latest;
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

/** An Environment's Configs with their files (no text) and where Services mount them: the canvas's trays and nodes. */
export function configsQuery(environment: EnvironmentRef): { query: "configs" } & ConfigsQuery {
  return { query: "configs", environment };
}

/** One Config with every file's text, references by Service name: its drawer. */
export function configQuery(environment: EnvironmentRef, config: string): { query: "config" } & ConfigItemQuery {
  return { query: "config", environment, config };
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

/** A Branch: its Parent, what it uses live, how much it has to sync and when it closes. Anything else is refused: not a Branch. */
export function branchQuery(environment: EnvironmentRef): { query: "branch" } & BranchQuery {
  return { query: "branch", environment };
}

/** What a Sync from `from` into `into` (by name) would carry, when the Store lands it: the Sync dialog's rows. */
export function syncQuery(from: EnvironmentRef, into: string): { query: "sync" } & SyncQuery {
  return { query: "sync", from, into: { project: from.project, environment: into } };
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
      return requireView(await readView(organizationSlug, deploymentsPage(environment, pageParam), signal));
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
 * Reads Store views together, prefetched by the page's loader (`prefetchStoreViews`). Each shows the marks of this
 * tab's pending writes over the committed view (`store-pending.ts`); a write's answer replaces both.
 */
export function useStoreViews<const Qs extends readonly ConfigQuery[]>(organizationSlug: string, queries: Qs):
  { [K in keyof Qs]: StoreResult<StoreViewOf<Qs[K]>> } {
  const scope = useCollectionScope();
  const results = useSuspenseQueries({ queries: queries.map((query) => storeViewOptions(organizationSlug, scope, query)) });
  const marks = usePendingMarks(organizationSlug);
  // SAFETY: `readView` checked each result against the query at its index; `map` types the tuple as an array.
  return results.map(({ data: result }, index) => {
    const query = queries[index];
    if (marks.length === 0 || !result.ok || !query || !("environment" in query)) return result;
    const key = environmentKey(query.environment);
    const mine = marks.filter((mark) => "environment" in mark && environmentKey(mark.environment) === key);
    if (mine.length === 0) return result;
    return { ok: true, value: showPending(result.value, mine) };
  }) as { [K in keyof Qs]: StoreResult<StoreViewOf<Qs[K]>> };
}

/** Reads one Store view; see `useStoreViews`. */
export function useStoreView<Q extends ConfigQuery>(organizationSlug: string, query: Q): StoreResult<StoreViewOf<Q>> {
  return useStoreViews(organizationSlug, [query] as const)[0];
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
