import "@tanstack/react-start/server-only";
import { X509Certificate } from "node:crypto";
import { isIPv6 } from "node:net";
import type { PublishCertificateMaterialRequest } from "@ployz/sdk";
import { and, eq } from "drizzle-orm";
import { Data, Effect } from "effect";
import { loadClusterDomain } from "#/modules/cluster-domain/cluster-domain.server";
import {
  HostedDnsError,
  type HostedDomainTarget,
  putHostedDomainRecords,
  renewHostedDomainLease,
  requestHostedDomainCertificate,
} from "#/modules/cluster-domain/hosted-dns.server";
import {
  type ClusterDomainTraffic,
  type IngressServerAddress,
  organizationClusterDomain,
} from "#/modules/cluster-domain/tables";
import { createWildcardCsr } from "#/modules/cluster-domain/wildcard-csr.server";
import { OrganizationRuntime, RUNTIME_FRAME_TIMEOUT_MS } from "#/modules/runtime/organization-runtime.server";
import { Database } from "#/server/database.server";
import { NotFound } from "#/server/public-error";
import { SecretEncryption } from "#/utils/encrypted-secret.server";

const PROBE_TIMEOUT_MS = 5_000;
/** Caddy answers it on port 80 of every ingress Server with the Machine id (core `INGRESS_VERIFY_PATH`). */
const INGRESS_VERIFY_PATH = "/.ployz-verify";
/** The wildcard is replaced once it has less than this left. */
const CERTIFICATE_RENEW_BEFORE_MS = 30 * 86_400_000;
/** A failed replacement with fewer days than this left is an operator error, not a warning. */
const CERTIFICATE_ALARM_BEFORE_DAYS = 14;

/**
 * Hosted DNS rejects the stored token (401), reaped the name (404) or retired it (410).
 * The name never changes, so retrying cannot help: an operator must.
 */
export class ClusterDomainUnusable extends Data.TaggedError("ClusterDomainUnusable")<{
  readonly name: string;
  readonly message: string;
}> {
  readonly retriable = false as const;
}

/**
 * What the sync found about the ingress Servers: the stored `ClusterDomainTraffic`, plus `unknown`
 * (a paired Cluster could not be reached or its frame could not be read) and the reachable Servers.
 */
export type IngressProbe =
  | { kind: "unknown" }
  | Exclude<ClusterDomainTraffic, { kind: "probed" }>
  | { kind: "probed"; reachable: IngressServerAddress[]; unreachable: IngressServerAddress[] };

/** Publishes Certificate Material to the Organization's Cluster. False when no Cluster is connected. */
const publishToCluster = Effect.fn("ClusterDomain.publishToCluster")(function* (
  organizationId: string,
  request: PublishCertificateMaterialRequest,
) {
  const session = yield* (yield* OrganizationRuntime).open(organizationId);
  if (session.status !== "connected") return false;
  yield* session.connected.publishCertificateMaterial(request);
  return true;
}, Effect.scoped);

/** Runs one bearer call against the Organization's reserved name; the sync only runs for an Organization that has one. */
export const withClusterDomain = <A, R>(
  organizationId: string,
  call: (target: HostedDomainTarget) => Effect.Effect<A, HostedDnsError, R>,
) => Effect.gen(function* () {
  const row = yield* loadClusterDomain(organizationId);
  if (!row) return yield* new NotFound({ message: "The Organization has no Cluster Domain." });
  const token = (yield* SecretEncryption).decrypt(row.encryptedToken);
  return yield* call({ endpoint: row.endpoint, name: row.name, token }).pipe(Effect.catchIf(
    (error) => error.status === 401 || error.status === 404 || error.status === 410,
    (error) => Effect.logError("Hosted DNS no longer accepts the Cluster Domain; an operator must restore it.", { name: row.name, status: error.status }).pipe(
      Effect.andThen(Effect.fail(new ClusterDomainUnusable({ name: row.name, message: `Hosted DNS answered ${error.status} for ${row.name}.` }))),
    ),
  ));
});

/** True when `GET http://<ip>/.ployz-verify` answers with the Machine id within five seconds. */
const probeIngressServer = (server: IngressServerAddress) => Effect.tryPromise(async (signal) => {
  const host = isIPv6(server.address) ? `[${server.address}]` : server.address;
  const response = await fetch(`http://${host}${INGRESS_VERIFY_PATH}`, {
    redirect: "manual",
    signal: AbortSignal.any([signal, AbortSignal.timeout(PROBE_TIMEOUT_MS)]),
  });
  return response.status === 200 && (await response.text()).trim() === server.machineId;
}).pipe(Effect.orElseSucceed(() => false));

/** Probes every ingress Server with a public IP in the Cluster's runtime frame. */
export const probeIngressServers = Effect.fn("ClusterDomain.probeIngressServers")(function* (organizationId: string) {
  const session = yield* (yield* OrganizationRuntime).open(organizationId);
  if (session.status === "no_connection") return { kind: "no_servers" } satisfies IngressProbe;
  if (session.status !== "connected") return { kind: "unknown" } satisfies IngressProbe;
  const frame = yield* session.connected.watchFirstFrame(RUNTIME_FRAME_TIMEOUT_MS);
  if (frame.machines.length === 0) return { kind: "no_servers" } satisfies IngressProbe;
  const servers = frame.machines.flatMap(({ machine }): IngressServerAddress[] =>
    machine.accepts_ingress && machine.public_ip !== null ? [{ machineId: machine.id, address: machine.public_ip }] : []);
  if (servers.length === 0) return { kind: "no_public_ip" } satisfies IngressProbe;
  const probed = yield* Effect.forEach(servers, (server) =>
    probeIngressServer(server).pipe(Effect.map((reachable) => ({ server, reachable }))), { concurrency: "unbounded" });
  return {
    kind: "probed",
    reachable: probed.flatMap(({ server, reachable }) => reachable ? [server] : []),
    unreachable: probed.flatMap(({ server, reachable }) => reachable ? [] : [server]),
  } satisfies IngressProbe;
}, Effect.scoped, Effect.catch((error) =>
  Effect.logWarning("No runtime frame for the Cluster Domain sync; records stay as published.", error)
    .pipe(Effect.as({ kind: "unknown" } as const))));

/** What a probe leaves on the row. */
function storedTraffic(probe: IngressProbe): ClusterDomainTraffic | null {
  switch (probe.kind) {
    case "unknown":
      return null;
    case "probed":
      return { kind: "probed", unreachable: probe.unreachable };
    case "no_servers":
    case "no_public_ip":
      return probe;
  }
}

/** Records what the probe found and when. An unknown probe clears the finding: an offline Cluster reads as ready. */
export const recordClusterDomainCheck = Effect.fn("ClusterDomain.recordCheck")(function* (
  organizationId: string,
  probe: IngressProbe,
) {
  const { drizzle } = yield* Database;
  const now = new Date();
  yield* drizzle.update(organizationClusterDomain).set({
    traffic: storedTraffic(probe),
    checkedAt: now,
    updatedAt: now,
  }).where(eq(organizationClusterDomain.organizationId, organizationId));
});

/** Points the apex at the reachable ingress Servers: a full-set PUT, which also renews the lease. */
export const publishClusterDomainRecords = Effect.fn("ClusterDomain.publishRecords")(function* (
  organizationId: string,
  reachable: IngressServerAddress[],
) {
  const addresses = reachable.map((server) => server.address);
  yield* withClusterDomain(organizationId, (target) => putHostedDomainRecords({
    ...target,
    a: addresses.filter((address) => !isIPv6(address)),
    aaaa: addresses.filter((address) => isIPv6(address)),
  }));
  const now = new Date();
  const { drizzle } = yield* Database;
  yield* drizzle.update(organizationClusterDomain).set({
    recordsSyncedAt: now,
    leaseRenewedAt: now,
    updatedAt: now,
  }).where(eq(organizationClusterDomain.organizationId, organizationId));
});

/** Renews the lease when no records were PUT, so an Organization with no reachable Cluster keeps its name. */
export const renewClusterDomainLease = Effect.fn("ClusterDomain.renewLease")(function* (organizationId: string) {
  yield* withClusterDomain(organizationId, renewHostedDomainLease);
  const now = new Date();
  const { drizzle } = yield* Database;
  yield* drizzle.update(organizationClusterDomain).set({ leaseRenewedAt: now, updatedAt: now })
    .where(eq(organizationClusterDomain.organizationId, organizationId));
});

/**
 * Replaces the wildcard for `name` and `*.name` when there is none or it has under 30 days left:
 * a fresh key and CSR, the chain from Hosted DNS, the key stored encrypted. A Hosted DNS failure
 * (a 429 included) keeps the current certificate; the next sync tries again. Under 14 days left
 * the failure is logged as an operator error.
 */
export const ensureClusterDomainCertificate = Effect.fn("ClusterDomain.ensureCertificate")(function* (organizationId: string) {
  const notAfter = (yield* loadClusterDomain(organizationId))?.certificateNotAfter ?? null;
  const msLeft = notAfter === null ? null : notAfter.getTime() - new Date().getTime();
  if (msLeft !== null && msLeft > CERTIFICATE_RENEW_BEFORE_MS) return false;
  const issued = yield* withClusterDomain(organizationId, ({ endpoint, name, token }) => {
    const { privateKeyPem, csrPem } = createWildcardCsr(name);
    return requestHostedDomainCertificate({ endpoint, name, token, csr: csrPem }).pipe(
      Effect.flatMap((chain) => Effect.try({
        // X509Certificate reads the first certificate of the chain: the leaf.
        try: () => ({ name, chain, privateKeyPem, notAfter: new Date(new X509Certificate(chain).validTo) }),
        catch: (cause) => new HostedDnsError({ operation: "request certificate", cause }),
      })),
    );
  }).pipe(Effect.catchTag("HostedDnsError", (error) => {
    const urgent = msLeft === null || msLeft < CERTIFICATE_ALARM_BEFORE_DAYS * 86_400_000;
    return (urgent
      ? Effect.logError(`Wildcard certificate issuance failed with under ${CERTIFICATE_ALARM_BEFORE_DAYS} days left; an operator must look.`, error)
      : Effect.logWarning("Wildcard certificate issuance failed; the current certificate stays.", error)
    ).pipe(Effect.as(null));
  }));
  if (issued === null) return false;
  const { drizzle } = yield* Database;
  yield* drizzle.update(organizationClusterDomain).set({
    encryptedCertificatePrivateKey: (yield* SecretEncryption).encrypt(issued.privateKeyPem),
    certificateChain: issued.chain,
    certificateNotAfter: issued.notAfter,
    updatedAt: new Date(),
  }).where(and(eq(organizationClusterDomain.organizationId, organizationId), eq(organizationClusterDomain.name, issued.name)));
  return true;
});

/**
 * Publishes the stored wildcard to the Cluster as `*.name`. Publishing is idempotent, so every sync
 * republishes: a re-paired Cluster that lacks the material gets it back. False when there is no
 * certificate yet or no Cluster connection.
 */
export const publishClusterDomainCertificate = Effect.fn("ClusterDomain.publishCertificate")(function* (organizationId: string) {
  const row = yield* loadClusterDomain(organizationId);
  if (!row?.certificateChain || !row.encryptedCertificatePrivateKey) return false;
  return yield* publishToCluster(organizationId, {
    hostname: `*.${row.name}`,
    change: {
      action: "set",
      certificate_chain_pem: row.certificateChain,
      private_key_pem: (yield* SecretEncryption).decrypt(row.encryptedCertificatePrivateKey),
    },
  });
});

/** Every Organization with a Cluster Domain, paired or not: the hourly sync's fan-out, so no lease lapses. */
export const listClusterDomainOrganizationIds = Effect.fn("ClusterDomain.listOrganizationIds")(function* () {
  const { drizzle } = yield* Database;
  const rows = yield* drizzle.select({ id: organizationClusterDomain.organizationId }).from(organizationClusterDomain);
  return rows.map((row) => row.id);
});
