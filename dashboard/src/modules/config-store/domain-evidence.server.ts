import "@tanstack/react-start/server-only";
import { Resolver } from "node:dns/promises";
import type { ClusterDomainStatus as StoreClusterDomainStatus, ConfigDomainEvidence, ConfigView, DnsLookup, PublishedHostname, RuntimeWatchView, TunnelAnswer, TunnelEvidence, TunnelReach } from "@ployz/sdk";
import { Array as Arr, Clock, Deferred, Effect, Option, Schema } from "effect";
import { activeCloudflare } from "#/modules/cloudflare/source.server";
import { reachesCluster } from "#/modules/cloudflare/tunnel-reach.server";
import { clusterDomainStatus } from "#/modules/cluster-domain/cluster-domain";
import { loadClusterDomain, reserveClusterDomain } from "#/modules/cluster-domain/cluster-domain.server";
import type { OrganizationClusterDomain } from "#/modules/cluster-domain/tables";
import { sendInngestEvent } from "#/modules/inngest/client";
import { createClusterDomainSyncRequestedEvent } from "#/modules/inngest/events";
import { requireClaimed } from "#/modules/identity/claim-required.server";
import { firstRuntimeFrame } from "#/modules/runtime/organization-runtime.server";
import { EnvironmentRef, environmentOf, type StoreCall, type StoreRead } from "./store.contract";
import { storeTry } from "#/modules/config-store/store-sdk.server";

const DNS_TIMEOUT_MS = 3_000;
/** How long one observation answers `domains` reads: an open drawer rereads them on every edit and every poll. */
const OBSERVATION_TTL_MS = 15_000;

/** The parts of a call that need domain evidence; decode it with this. The Store validates the whole call. */
const DomainCall = Schema.Union([
  Schema.Struct({ command: Schema.Literals(["add_domain", "remove_domain", "set_generated_domain"]) }),
  // A Branch's name lengthens the generated domains it copies: the Store checks them under the Cluster Domain.
  Schema.Struct({ command: Schema.Literals(["create_branch", "copy_node", "sync", "take"]) }),
  // Only a Deploy expands domains: a removal expands none, a retry inherits what its source froze.
  Schema.Struct({ command: Schema.Literal("admit"), admit: Schema.Literal("deploy"), environment: Schema.optional(EnvironmentRef) }),
  Schema.Struct({ query: Schema.Literals(["domains", "domain"]) }),
  // A change's views name the generated domains it stages under the Cluster Domain.
  Schema.Struct({ query: Schema.Literals(["diff", "plan", "service"]) }),
]);

const AddDomainAnyHostname = Schema.Struct({ command: Schema.Literal("add_domain"), hostname: Schema.optional(Schema.Unknown) });

const nothing: ConfigDomainEvidence = { cluster_domain: null, certificates: null, ingress_addresses: [], lookups: [] };

/** The Cluster Domain as the Store reads it: its name and what the last sync found. */
function clusterDomain(row: OrganizationClusterDomain | null): ConfigDomainEvidence["cluster_domain"] {
  if (row === null) return null;
  // A Server left out while others carry traffic is the Cluster Domain section's to show, not every Domain's.
  const { state } = clusterDomainStatus(row, new Date());
  const seen: StoreClusterDomainStatus = state.kind !== "attention"
    ? { kind: state.kind }
    : state.reason === "port_80" || state.reason === "port_80_taken"
    ? { kind: state.reason, addresses: [...state.addresses] }
    : state.reason === "ingress_down"
    ? { kind: state.reason, servers: [...state.servers] }
    : { kind: state.reason };
  return { name: row.name, status: seen };
}

/** The Cloudflare zone and tunnel target while the Cluster is served through Cloudflare, else null. */
type Cloudflare = Effect.Success<ReturnType<typeof activeCloudflare>>;

/**
 * The Cluster Domain the Store expands generated names under. Behind Cloudflare it is the user's zone, always ready,
 * and reserves nothing; otherwise it is the Hosted name, reserved first when the call generates a domain.
 */
const clusterDomainOf = Effect.fn("ConfigStore.clusterDomainOf")(function* (organizationId: string, cloudflare: Cloudflare, reserve: boolean) {
  if (cloudflare !== null) return { name: cloudflare.zone, status: { kind: "ready" } } satisfies NonNullable<ConfigDomainEvidence["cluster_domain"]>;
  return clusterDomain(reserve ? yield* reserveClusterDomain(organizationId) : yield* loadClusterDomain(organizationId));
});

/**
 * The hostnames the Servers publish now, each with the Namespace and Service publishing it. As in the runtime's
 * `hostname_owners`, only Service containers claim one: a pre-deploy hook keeps its spec's ports but serves nothing.
 */
export function publishedOf(frame: RuntimeWatchView): PublishedHostname[] {
  const seen = new Map<string, PublishedHostname>();
  for (const container of frame.containers) {
    if (container.kind !== "service_container") continue;
    for (const port of container.resolved_spec.ports) {
      if (port.mode === "ingress") seen.set(`${container.namespace}/${port.hostname}`, { hostname: port.hostname, namespace: container.namespace, service: container.resolved_spec.name });
    }
  }
  return [...seen.values()];
}

/**
 * What one Runtime Watch frame says of the domains: the Cluster's certificates and those it couldn't read, how many
 * Servers take ingress when it could read every Machine and their public addresses, and the hostnames its Services publish.
 */
export function observedOf(frame: RuntimeWatchView) {
  const ingress = frame.machines.filter(({ machine }) => machine.accepts_ingress);
  return {
    certificates: frame.certificates,
    incomplete_certificates: frame.incomplete_ids.certificates,
    ingress_servers: frame.incomplete_ids.machines.length === 0 ? ingress.length : undefined,
    ingress_addresses: ingress.flatMap(({ machine }) => machine.public_ip !== null ? [machine.public_ip] : []),
    published: publishedOf(frame),
  };
}

/** What one Runtime Watch frame gave the domains, with the Machine ids that tell Cloud a fetch reached this Cluster. */
export type Observed = { readonly seen: ReturnType<typeof observedOf>; readonly machineIds: ReadonlySet<string> };

/** The Cluster's {@link observedOf}; nothing when no Cluster answers, so the Store reads its certificates as unobserved. */
const observeCluster = Effect.fn("ConfigStore.observeCluster")(function* (organizationId: string) {
  const frame = yield* firstRuntimeFrame(organizationId);
  return frame === null ? null : { seen: observedOf(frame), machineIds: new Set(frame.machines.map(({ machine }) => String(machine.id))) } satisfies Observed;
}, Effect.catch((error) =>
  Effect.logWarning("No runtime frame for domain statuses; they read as unobserved.", error).pipe(Effect.as(null))));

// ponytail: one memo per Cloud process, never evicted; bounded by Organizations, and a TTL this short needs no sharing.
const observations = new Map<string, { at: number; observed: Observed | null }>();

/** The Cluster as last observed within the TTL, else observed now; a check (`fresh`) always observes now. */
const observeClusterRecently = (organizationId: string, fresh: boolean) => Effect.gen(function* () {
  const now = yield* Clock.currentTimeMillis;
  const last = observations.get(organizationId);
  if (!fresh && last !== undefined && now - last.at < OBSERVATION_TTL_MS) return last.observed;
  const observed = yield* observeCluster(organizationId);
  observations.set(organizationId, { at: now, observed });
  return observed;
});

/** How old an observation's published hostnames may be for a domain edit, which never waits on the Cluster (E14). */
const PUBLISHED_TTL_MS = 5 * 60_000;

/** The hostnames the Cluster published when last observed recently; null when it wasn't, and the Store refuses nothing for them. */
const recentlyPublished = (organizationId: string) => Effect.map(Clock.currentTimeMillis, (now) => {
  const last = observations.get(organizationId);
  return last?.observed && now - last.at < PUBLISHED_TTL_MS ? last.observed.seen.published : null;
});

/**
 * What a Store's own event (`system`) carries: the Cluster Domain its writes check names under, and the published
 * hostnames a Deploy is gated on from the recent observation, else none.
 */
export const systemDomainEvidence = Effect.fn("ConfigStore.systemDomainEvidence")(function* (organizationId: string) {
  const evidence: ConfigDomainEvidence = { ...nothing, cluster_domain: yield* clusterDomainNow(organizationId) };
  const published = yield* recentlyPublished(organizationId);
  if (published) evidence.published = published;
  return evidence;
});

/** The Cluster Domain as Cloud knows it now, reserving none: null until a generated domain reserves one. */
const clusterDomainNow = Effect.fn("ConfigStore.clusterDomainNow")(function* (organizationId: string) {
  return yield* clusterDomainOf(organizationId, yield* activeCloudflare(organizationId), false);
});

/** A name that doesn't resolve answers nothing; any other failure (a timeout, SERVFAIL) is no answer at all. */
const absent = (error: NodeJS.ErrnoException) =>
  error.code === "ENOTFOUND" || error.code === "ENODATA" ? [] : Promise.reject(error);

/** What DNS answers for `hostname` now, or nothing when DNS couldn't answer: the Store reads it as unobserved. */
export const lookUpHostname = (hostname: string) => Effect.promise(async (): Promise<DnsLookup | null> => {
  const resolver = new Resolver({ timeout: DNS_TIMEOUT_MS, tries: 1 });
  const [cnames, v4, v6] = await Promise.all([
    resolver.resolveCname(hostname).catch(absent),
    resolver.resolve4(hostname).catch(absent),
    resolver.resolve6(hostname).catch(absent),
  ]).catch(() => [null, null, null] as const);
  if (cnames === null || v4 === null || v6 === null) return null;
  return { hostname, cname: cnames[0] ?? null, addresses: [...v4, ...v6] };
}).pipe(Effect.tap((found) => found === null
  ? Effect.logWarning("DNS didn't answer; the domain's DNS reads as unobserved.", { hostname })
  : Effect.void));

/**
 * How long a fetch through a hostname answers `domains` reads, as {@link OBSERVATION_TTL_MS} does for the Cluster.
 * One nobody answered is asked again sooner: the fault may be passing, and the row says what to fix meanwhile.
 */
const REACH_TTL_MS = 15_000;
const UNANSWERED_TTL_MS = 5_000;
/** How many hostnames one read fetches through at most: the ones never observed first, then the longest unobserved, so later reads reach the rest. */
const REACH_LIMIT = 16;
/** How long a read waits for the first fetch through a name. One that takes longer lands in the memo and answers the next read; a check waits it out. */
const REACH_WAIT_MS = 250;
// ponytail: as `observations`, one memo per process, bounded by the custom domains Organizations publish.
const reaches = new Map<string, { at: number; answer: TunnelAnswer }>();
/** The fetches under way, so reads that overlap share one fetch per hostname instead of each making their own. */
const fetching = new Map<string, Deferred.Deferred<TunnelAnswer>>();

/** The fetch under way for `key`, started now when none is. It runs on its own: its answer is memoised when it lands, whether or not a read still waits. */
const fetchOnce = (key: string, fetch: Effect.Effect<TunnelAnswer>) => Effect.gen(function* () {
  const shared = fetching.get(key);
  if (shared !== undefined) return shared;
  const mine = yield* Deferred.make<TunnelAnswer>();
  fetching.set(key, mine);
  yield* fetch.pipe(
    Effect.tap((answer) => Effect.map(Clock.currentTimeMillis, (at) => { reaches.set(key, { at, answer }); })),
    Effect.onExit((exit) => {
      fetching.delete(key);
      return Deferred.done(mine, exit);
    }),
    Effect.forkDetach({ startImmediately: true }),
  );
  return mine;
});

/**
 * Who answers through each of `hostnames` the Servers publish: DNS can't say, since Cloudflare hides a proxied CNAME,
 * so Cloud fetches the verify path through it, as the Servers do on the Public Door. A name inside the zone is fetched
 * too: the wildcard record covers it only while no record of its own sends it elsewhere. A read asks only for the
 * custom hostnames it judges, and every answer it has serves, however old; the fetches it starts, {@link REACH_LIMIT}
 * at most, go to the names whose answer is missing or past its time, the longest unobserved first. No fetch holds the
 * page: an answer past its time serves the read while a fetch refreshes it for the next one, and a name never
 * answered waits {@link REACH_WAIT_MS} before it reads as unobserved. A check (`fresh`) fetches again rather than
 * reusing an answer from seconds ago, and waits for it. No address of a Server is looked at in this mode.
 */
export const tunnelEvidence = Effect.fn("ConfigStore.tunnelEvidence")(function* (
  organizationId: string,
  cloudflare: NonNullable<Cloudflare>,
  observed: Observed | null,
  fresh: boolean,
  hostnames: readonly string[],
  reach: typeof reachesCluster = reachesCluster,
) {
  if (observed === null) return { target: cloudflare.target, reached: [] } satisfies TunnelEvidence;
  const now = yield* Clock.currentTimeMillis;
  const published = new Set(observed.seen.published.map(({ hostname }) => String(hostname)));
  const looked = [...new Set(hostnames)].filter((hostname) => published.has(hostname));
  const lastOf = (hostname: string) => reaches.get(`${organizationId}/${hostname}`);
  const due = (last: { at: number; answer: TunnelAnswer } | undefined) =>
    fresh || last === undefined || now - last.at >= (last.answer === "unanswered" ? UNANSWERED_TTL_MS : REACH_TTL_MS);
  const fetches = new Set(
    looked.filter((hostname) => due(lastOf(hostname))).sort((a, b) => (lastOf(a)?.at ?? -1) - (lastOf(b)?.at ?? -1)).slice(0, REACH_LIMIT),
  );
  const answers = yield* Effect.forEach(looked, (hostname) => Effect.gen(function* () {
    const key = `${organizationId}/${hostname}`;
    const last = reaches.get(key);
    if (!fetches.has(hostname)) return last === undefined ? Option.none() : Option.some({ hostname, answer: last.answer } satisfies TunnelReach);
    const pending = yield* fetchOnce(key, reach(hostname, observed.machineIds));
    // A stale answer serves this read while the fetch under way lands for the next one.
    if (!fresh && last !== undefined) return Option.some({ hostname, answer: last.answer } satisfies TunnelReach);
    const answer = yield* Deferred.await(pending).pipe(fresh ? Effect.map(Option.some) : Effect.timeoutOption(REACH_WAIT_MS));
    return Option.map(answer, (answer) => ({ hostname, answer } satisfies TunnelReach));
  }), { concurrency: "unbounded" });
  return { target: cloudflare.target, reached: Arr.getSomes(answers) } satisfies TunnelEvidence;
});

/** The custom hostnames a domains or domain view lists. */
const customHostnamesOf = (view: Extract<ConfigView, { view: "domains" | "domain" }>) =>
  (view.view === "domains" ? view.domains : [view.domain]).flatMap((domain) => domain.kind === "custom" ? [String(domain.hostname)] : []);

/**
 * What Cloud observes of the Organization's public domains, for the Store calls that need it. Staging or admitting a generated domain reserves the Cluster Domain first; reading domains
 * observes the Cluster (or reuses an observation from the last few seconds); checking one also looks up its DNS now and asks for a Cluster Domain sync.
 * Behind Cloudflare a read also says who answers through each custom hostname it lists, and no DNS lookup of a Server's address is made. Nothing here comes
 * from the caller.
 */
export const gatherDomainEvidence = Effect.fn("ConfigStore.gatherDomainEvidence")(function* (
  organizationId: string,
  call: StoreCall,
  read: StoreRead,
  lookUp: (hostname: string) => Effect.Effect<DnsLookup | null> = lookUpHostname,
) {
  const asked = call.operation === "read" ? call.query : call.command;
  const adding = Option.getOrUndefined(Schema.decodeUnknownOption(AddDomainAnyHostname)(asked));
  if ((adding?.hostname ?? null) !== null) yield* requireClaimed(organizationId, "add_domain");
  const wanted = Option.getOrUndefined(Schema.decodeUnknownOption(DomainCall)(asked));
  if (wanted === undefined) {
    const checksNames = call.operation === "write" || call.query.query === "history_preview";
    return checksNames ? { ...nothing, cluster_domain: yield* clusterDomainNow(organizationId) } : nothing;
  }
  const cloudflare = yield* activeCloudflare(organizationId);
  if ("command" in wanted) {
    if (wanted.command !== "admit") {
      const generates = wanted.command === "set_generated_domain" || (adding !== undefined && (adding.hostname ?? null) === null);
      const edit: ConfigDomainEvidence = { ...nothing, cluster_domain: yield* clusterDomainOf(organizationId, cloudflare, generates) };
      const published = yield* recentlyPublished(organizationId);
      if (published) edit.published = published;
      // Behind Cloudflare the zone is the user's own, so a custom domain may sit in it.
      if (cloudflare !== null) edit.tunnel = { target: cloudflare.target, reached: [] };
      return edit;
    }
    const domains = yield* storeTry(() => read({ query: "domains", environment: environmentOf(wanted.environment), service: null })).pipe(
      Effect.option,
    );
    const generated = Option.getOrUndefined(domains)?.domains.some((domain) => domain.kind === "generated") ?? false;
    // A Deploy refuses a hostname another Namespace publishes: it looks at the Cluster (or a look from seconds ago).
    const deploy: ConfigDomainEvidence = { ...nothing, cluster_domain: yield* clusterDomainOf(organizationId, cloudflare, generated) };
    const observed = generated ? yield* observeClusterRecently(organizationId, false) : null;
    if (observed) deploy.published = observed.seen.published;
    return deploy;
  }
  const named: ConfigDomainEvidence = { ...nothing, cluster_domain: yield* clusterDomainOf(organizationId, cloudflare, false) };
  const query = call.operation === "read" ? call.query : null;
  if (query === null || (query.query !== "domains" && query.query !== "domain")) return named;
  const fresh = query.query === "domain";
  const observed = yield* observeClusterRecently(organizationId, fresh);
  const evidence: ConfigDomainEvidence = { ...named, ...observed?.seen };
  // The Store's own rows name the custom hostnames this read judges; a read without evidence gathers none of its own.
  const rows = Option.getOrUndefined(yield* storeTry(() => read(query)).pipe(Effect.option));
  if (cloudflare !== null) {
    // Behind Cloudflare no visitor reaches a Server's address, so none is offered to compare a DNS answer with.
    evidence.ingress_addresses = [];
    evidence.tunnel = yield* tunnelEvidence(organizationId, cloudflare, observed, fresh, rows === undefined ? [] : customHostnamesOf(rows));
  }
  if (query.query === "domains") return evidence;
  // A check refreshes what Cloud knows: the Cluster Domain's records and certificate, and this domain's DNS.
  yield* sendInngestEvent(createClusterDomainSyncRequestedEvent({ organizationId })).pipe(
    Effect.catch((error) => Effect.logWarning("Cluster Domain sync request failed; the hourly sync covers it.", error)),
  );
  if (cloudflare !== null || rows?.view !== "domain" || rows.domain.kind !== "custom") return evidence;
  const lookup = yield* lookUp(rows.domain.hostname);
  return lookup === null ? evidence : { ...evidence, lookups: [lookup] };
});
