import "@tanstack/react-start/server-only";
import { Resolver } from "node:dns/promises";
import type { ClusterDomainStatus as StoreClusterDomainStatus, ConfigDomainEvidence, ConfigQuery, ConfigView, DnsLookup } from "@ployz/sdk";
import { Effect, Option, Schema } from "effect";
import { customDomainsAllowed } from "#/modules/billing/custom-domain-capability";
import { clusterDomainStatus } from "#/modules/cluster-domain/cluster-domain";
import { loadClusterDomain, reserveClusterDomain } from "#/modules/cluster-domain/cluster-domain.server";
import type { OrganizationClusterDomain } from "#/modules/cluster-domain/tables";
import { sendInngestEvent } from "#/modules/inngest/client";
import { createClusterDomainSyncRequestedEvent } from "#/modules/inngest/events";
import { OrganizationRuntime, RUNTIME_FRAME_TIMEOUT_MS } from "#/modules/runtime/organization-runtime.server";
import type { StoreCall } from "./store.contract";

const DNS_TIMEOUT_MS = 3_000;

/** The parts of a call that need domain evidence; decode it with this. The Store validates the whole call. */
const EnvironmentRef = Schema.Struct({
  project: Schema.optional(Schema.NullOr(Schema.String)),
  environment: Schema.optional(Schema.NullOr(Schema.String)),
});
const DomainCall = Schema.Union([
  Schema.Struct({ command: Schema.Literals(["add_domain", "remove_domain"]) }),
  Schema.Struct({ command: Schema.Literal("admit"), environment: Schema.optional(EnvironmentRef) }),
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
}, Effect.scoped, Effect.catch((error) =>
  Effect.logWarning("No runtime frame for domain statuses; they read as unobserved.", error).pipe(Effect.as(null))));

/** What DNS answers for `hostname` now. A name that doesn't resolve answers nothing. */
export const lookUpHostname = (hostname: string) => Effect.promise(async (): Promise<DnsLookup> => {
  const resolver = new Resolver({ timeout: DNS_TIMEOUT_MS, tries: 1 });
  const none = () => [];
  const [cnames, v4, v6] = await Promise.all([
    resolver.resolveCname(hostname).catch(none),
    resolver.resolve4(hostname).catch(none),
    resolver.resolve6(hostname).catch(none),
  ]);
  return { hostname, cname: cnames[0] ?? null, addresses: [...v4, ...v6] };
});

/**
 * What Cloud observes of the Organization's public domains, for the Store calls that need it. Adding a domain gets
 * the custom-domain capability; admitting a generated domain reserves the Cluster Domain first; reading domains
 * observes the Cluster; checking one also looks up its DNS now and asks for a Cluster Domain sync. Nothing here comes
 * from the caller.
 */
export const gatherDomainEvidence = Effect.fn("ConfigStore.gatherDomainEvidence")(function* (
  organizationId: string,
  call: StoreCall,
  read: (query: ConfigQuery) => Promise<ConfigView>,
  lookUp: (hostname: string) => Effect.Effect<DnsLookup> = lookUpHostname,
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
    const domains = yield* Effect.tryPromise(() => read({
      query: "domains",
      environment: { project: wanted.environment?.project ?? null, environment: wanted.environment?.environment ?? null },
      service: null,
    })).pipe(Effect.option);
    const view = Option.getOrUndefined(domains);
    const generated = view?.view === "domains" && view.domains.some((domain) => domain.kind === "generated");
    const row = generated ? yield* reserveClusterDomain(organizationId) : yield* loadClusterDomain(organizationId);
    return { ...nothing, cluster_domain: clusterDomain(row) };
  }
  const evidence: ConfigDomainEvidence = {
    ...nothing,
    cluster_domain: clusterDomain(yield* loadClusterDomain(organizationId)),
    ...(yield* observeCluster(organizationId)),
  };
  if (wanted.query === "domains") return evidence;
  // A check refreshes what Cloud knows: the Cluster Domain's records and certificate, and this domain's DNS.
  yield* sendInngestEvent(createClusterDomainSyncRequestedEvent({ organizationId })).pipe(
    Effect.catch((error) => Effect.logWarning("Cluster Domain sync request failed; the hourly sync covers it.", error)),
  );
  const found = Option.getOrUndefined(yield* Effect.tryPromise(() => read({
    query: "domain",
    environment: { project: wanted.environment?.project ?? null, environment: wanted.environment?.environment ?? null },
    domain: wanted.domain,
  })).pipe(Effect.option));
  if (found?.view !== "domain" || found.domain.kind !== "custom") return evidence;
  return { ...evidence, lookups: [yield* lookUp(found.domain.hostname)] };
});
