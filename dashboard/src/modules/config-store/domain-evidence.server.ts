import "@tanstack/react-start/server-only";
import { Resolver } from "node:dns/promises";
import type { ClusterDomainStatus as StoreClusterDomainStatus, ConfigDomainEvidence, DnsLookup } from "@ployz/sdk";
import { Clock, Effect, Option, Schema } from "effect";
import { customDomainsAllowed } from "#/modules/billing/custom-domain-capability";
import { clusterDomainStatus } from "#/modules/cluster-domain/cluster-domain";
import { loadClusterDomain, reserveClusterDomain } from "#/modules/cluster-domain/cluster-domain.server";
import type { OrganizationClusterDomain } from "#/modules/cluster-domain/tables";
import { sendInngestEvent } from "#/modules/inngest/client";
import { createClusterDomainSyncRequestedEvent } from "#/modules/inngest/events";
import { OrganizationRuntime, RUNTIME_FRAME_TIMEOUT_MS } from "#/modules/runtime/organization-runtime.server";
import { EnvironmentRef, environmentOf, type StoreCall, type StoreRead } from "./store.contract";
import { storeTry } from "#/modules/config-store/store-sdk.server";

const DNS_TIMEOUT_MS = 3_000;
/** A slow or unreachable Cluster leaves statuses unobserved rather than holding up the page that reads them. */
const OBSERVE_TIMEOUT = "5 seconds";
/** How long one observation answers `domains` reads: an open drawer rereads them on every edit and every poll. */
const OBSERVATION_TTL_MS = 15_000;

/** The parts of a call that need domain evidence; decode it with this. The Store validates the whole call. */
const DomainCall = Schema.Union([
  Schema.Struct({ command: Schema.Literals(["add_domain", "remove_domain"]) }),
  // Only a Deploy expands domains: a removal expands none, a retry inherits what its source froze.
  Schema.Struct({ command: Schema.Literal("admit"), admit: Schema.Literal("deploy"), environment: Schema.optional(EnvironmentRef) }),
  Schema.Struct({ query: Schema.Literal("domains") }),
  Schema.Struct({ query: Schema.Literal("domain"), environment: Schema.optional(EnvironmentRef), domain: Schema.String }),
]);

const nothing: ConfigDomainEvidence = { custom_domains: false, cluster_domain: null, certificates: null, ingress_addresses: [], lookups: [] };

/** The Cluster Domain as the Store reads it: its name and what the last sync found. */
function clusterDomain(row: OrganizationClusterDomain | null): ConfigDomainEvidence["cluster_domain"] {
  if (row === null) return null;
  const status = clusterDomainStatus(row, new Date());
  const seen: StoreClusterDomainStatus = status.kind !== "attention"
    ? { kind: status.kind }
    : status.reason === "port_80" ? { kind: "port_80", addresses: [...status.addresses] } : { kind: status.reason };
  return { name: row.name, status: seen };
}

/**
 * One Runtime Watch frame: the Cluster's certificates and its ingress Servers' public addresses. Nothing when no
 * Cluster answers, so the Store reads its certificates as unobserved.
 */
const observeCluster = Effect.fn("ConfigStore.observeCluster")(function* (organizationId: string) {
  const session = yield* (yield* OrganizationRuntime).open(organizationId);
  if (session.status !== "connected") return null;
  const frame = yield* session.connected.watchFirstFrame(RUNTIME_FRAME_TIMEOUT_MS);
  return {
    certificates: frame.certificates,
    ingress_addresses: frame.machines.flatMap(({ machine }) =>
      machine.accepts_ingress && machine.public_ip !== null ? [machine.public_ip] : []),
  };
}, Effect.scoped, Effect.timeout(OBSERVE_TIMEOUT), Effect.catch((error) =>
  Effect.logWarning("No runtime frame for domain statuses; they read as unobserved.", error).pipe(Effect.as(null))));

type ClusterObservation = Effect.Success<ReturnType<typeof observeCluster>>;
// ponytail: one memo per Cloud process, never evicted; bounded by Organizations, and a TTL this short needs no sharing.
const observations = new Map<string, { at: number; seen: ClusterObservation }>();

/** The Cluster as last observed within the TTL, else observed now; a check (`fresh`) always observes now. */
const observeClusterRecently = (organizationId: string, fresh: boolean) => Effect.gen(function* () {
  const now = yield* Clock.currentTimeMillis;
  const last = observations.get(organizationId);
  if (!fresh && last !== undefined && now - last.at < OBSERVATION_TTL_MS) return last.seen;
  const seen = yield* observeCluster(organizationId);
  observations.set(organizationId, { at: now, seen });
  return seen;
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
 * What Cloud observes of the Organization's public domains, for the Store calls that need it. Adding a domain gets
 * the custom-domain capability; admitting a generated domain reserves the Cluster Domain first; reading domains
 * observes the Cluster (or reuses an observation from the last few seconds); checking one also looks up its DNS now and asks for a Cluster Domain sync. Nothing here comes
 * from the caller.
 */
export const gatherDomainEvidence = Effect.fn("ConfigStore.gatherDomainEvidence")(function* (
  organizationId: string,
  call: StoreCall,
  read: StoreRead,
  lookUp: (hostname: string) => Effect.Effect<DnsLookup | null> = lookUpHostname,
) {
  const wanted = Option.getOrUndefined(Schema.decodeUnknownOption(DomainCall)(call.operation === "read" ? call.query : call.command));
  if (wanted === undefined) return nothing;
  if ("command" in wanted) {
    if (wanted.command !== "admit") {
      return {
        ...nothing,
        custom_domains: wanted.command === "add_domain" && (yield* customDomainsAllowed(organizationId)),
        cluster_domain: clusterDomain(yield* loadClusterDomain(organizationId)),
      };
    }
    const domains = yield* storeTry(() => read({ query: "domains", environment: environmentOf(wanted.environment), service: null })).pipe(
      Effect.option,
    );
    const generated = Option.getOrUndefined(domains)?.domains.some((domain) => domain.kind === "generated") ?? false;
    const row = generated ? yield* reserveClusterDomain(organizationId) : yield* loadClusterDomain(organizationId);
    return { ...nothing, cluster_domain: clusterDomain(row) };
  }
  const evidence: ConfigDomainEvidence = {
    ...nothing,
    cluster_domain: clusterDomain(yield* loadClusterDomain(organizationId)),
    ...(yield* observeClusterRecently(organizationId, wanted.query === "domain")),
  };
  if (wanted.query === "domains") return evidence;
  // A check refreshes what Cloud knows: the Cluster Domain's records and certificate, and this domain's DNS.
  yield* sendInngestEvent(createClusterDomainSyncRequestedEvent({ organizationId })).pipe(
    Effect.catch((error) => Effect.logWarning("Cluster Domain sync request failed; the hourly sync covers it.", error)),
  );
  const found = Option.getOrUndefined(yield* storeTry(() => read({
    query: "domain", environment: environmentOf(wanted.environment), domain: wanted.domain,
  })).pipe(Effect.option));
  if (found?.domain.kind !== "custom") return evidence;
  const lookup = yield* lookUp(found.domain.hostname);
  return lookup === null ? evidence : { ...evidence, lookups: [lookup] };
});
