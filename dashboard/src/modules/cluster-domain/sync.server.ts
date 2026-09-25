import "@tanstack/react-start/server-only";
import { X509Certificate } from "node:crypto";
import { isIPv6 } from "node:net";
import type { PublishCertificateMaterialRequest } from "@ployz/sdk";
import { and, eq } from "drizzle-orm";
import { Data, Effect } from "effect";
import { loadClusterDomain, reserveClusterDomain } from "#/modules/cluster-domain/cluster-domain.server";
import {
  HostedDnsError,
  type HostedDomainTarget,
  putHostedDomainRecords,
  renewHostedDomainLease,
  requestHostedDomainCertificate,
} from "#/modules/cluster-domain/hosted-dns.server";
import {
  type IngressServerAddress,
  organizationClusterDomain,
  type OrganizationClusterDomain,
} from "#/modules/cluster-domain/tables";
import { createWildcardCsr } from "#/modules/cluster-domain/wildcard-csr.server";
import { OrganizationRuntime } from "#/modules/runtime/organization-runtime.server";
import { Database } from "#/server/database.server";
import { NotFound } from "#/server/public-error";
import { SecretEncryption } from "#/utils/encrypted-secret.server";

const RUNTIME_FRAME_TIMEOUT_MS = 10_000;
const PROBE_TIMEOUT_MS = 5_000;
/** Caddy answers it on port 80 of every ingress Server with the Machine id (core `INGRESS_VERIFY_PATH`). */
const INGRESS_VERIFY_PATH = "/.ployz-verify";
/** The wildcard is replaced once it has less than this left. */
const CERTIFICATE_RENEW_BEFORE_MS = 30 * 86_400_000;

/** Hosted DNS rejects the stored token (401). Retrying cannot help. */
export class ClusterDomainUnusable extends Data.TaggedError("ClusterDomainUnusable")<{
  readonly name: string;
  readonly message: string;
}> {
  readonly retriable = false as const;
}

export type IngressProbe = {
  readonly reachable: IngressServerAddress[];
  readonly unreachable: IngressServerAddress[];
};

/** Sets or clears Certificate Material on the Organization's Cluster. False when no Cluster is connected. */
const publishToCluster = Effect.fn("ClusterDomain.publishToCluster")(function* (
  organizationId: string,
  request: PublishCertificateMaterialRequest,
) {
  const session = yield* (yield* OrganizationRuntime).open(organizationId);
  if (session.status !== "connected") return false;
  yield* session.connected.publishCertificateMaterial(request);
  return true;
}, Effect.scoped);

/**
 * Runs one bearer call against the Organization's reserved name; the sync's `reserve` step made the row.
 * A name Hosted DNS reaped (404) or retired (410) is forgotten, its wildcard cleared from the Cluster,
 * and a fresh name reserved for one retry, so deploys never ship a dead name.
 */
export const withClusterDomain = <A, R>(
  organizationId: string,
  call: (target: HostedDomainTarget) => Effect.Effect<A, HostedDnsError, R>,
) => Effect.gen(function* () {
  const encryption = yield* SecretEncryption;
  const { drizzle } = yield* Database;
  const attempt = (row: OrganizationClusterDomain) =>
    call({ endpoint: row.endpoint, name: row.name, token: encryption.decrypt(row.encryptedToken) }).pipe(
      Effect.mapError((error) => error.status === 401
        ? new ClusterDomainUnusable({ name: row.name, message: `Hosted DNS rejected the token for ${row.name}.` })
        : error),
    );
  const row = yield* loadClusterDomain(organizationId);
  if (!row) return yield* new NotFound({ message: "The Organization has no Cluster Domain." });
  return yield* attempt(row).pipe(Effect.catchIf(
    (error): error is HostedDnsError => error._tag === "HostedDnsError" && (error.status === 404 || error.status === 410),
    (error) => Effect.gen(function* () {
      yield* drizzle.delete(organizationClusterDomain)
        .where(and(eq(organizationClusterDomain.organizationId, organizationId), eq(organizationClusterDomain.name, row.name)));
      if (error.status === 410) {
        yield* Effect.logError("Hosted DNS retired the Cluster Domain; reserving a new one. Services need a redeploy to use it.", { name: row.name });
      } else {
        yield* Effect.logInfo("Hosted DNS reaped the Cluster Domain; reserving a new one.", { name: row.name });
      }
      const replacement = yield* reserveClusterDomain(organizationId);
      if (replacement.name !== row.name) {
        yield* publishToCluster(organizationId, { hostname: `*.${row.name}`, change: { action: "clear" } }).pipe(
          Effect.catch((cause) => Effect.logWarning("Clearing the replaced Cluster Domain's wildcard failed.", cause)),
        );
      }
      return yield* attempt(replacement);
    }),
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

/**
 * Probes every ingress Server with a public IP in the Cluster's runtime frame.
 * Null when no frame could be read: the caller then leaves the published records alone.
 */
export const probeIngressServers = Effect.fn("ClusterDomain.probeIngressServers")(function* (organizationId: string) {
  const session = yield* (yield* OrganizationRuntime).open(organizationId);
  if (session.status !== "connected") return null;
  const frame = yield* session.connected.watchFirstFrame(RUNTIME_FRAME_TIMEOUT_MS);
  const servers = frame.machines.flatMap(({ machine }): IngressServerAddress[] =>
    machine.accepts_ingress && machine.public_ip !== null ? [{ machineId: machine.id, address: machine.public_ip }] : []);
  const probed = yield* Effect.forEach(servers, (server) =>
    probeIngressServer(server).pipe(Effect.map((reachable) => ({ server, reachable }))), { concurrency: "unbounded" });
  return {
    reachable: probed.flatMap(({ server, reachable }) => reachable ? [server] : []),
    unreachable: probed.flatMap(({ server, reachable }) => reachable ? [] : [server]),
  } satisfies IngressProbe;
}, Effect.scoped, Effect.catch((error) =>
  Effect.logWarning("No runtime frame for the Cluster Domain sync; records stay as published.", error).pipe(Effect.as(null))));

/**
 * Points the apex at the reachable ingress Servers (a full-set PUT, which also renews the lease) and
 * records the probe. An empty reachable set writes no records: Hosted DNS refuses it, and the last good
 * set is better than none. True when the records were PUT.
 */
export const publishClusterDomainRecords = Effect.fn("ClusterDomain.publishRecords")(function* (
  organizationId: string,
  probe: IngressProbe,
) {
  const { drizzle } = yield* Database;
  const ofOrganization = eq(organizationClusterDomain.organizationId, organizationId);
  if (probe.reachable.length === 0) {
    yield* drizzle.update(organizationClusterDomain).set({ unreachable: probe.unreachable, updatedAt: new Date() }).where(ofOrganization);
    return false;
  }
  const addresses = probe.reachable.map((server) => server.address);
  yield* withClusterDomain(organizationId, (target) => putHostedDomainRecords({
    ...target,
    a: addresses.filter((address) => !isIPv6(address)),
    aaaa: addresses.filter((address) => isIPv6(address)),
  }));
  const now = new Date();
  yield* drizzle.update(organizationClusterDomain).set({
    recordsSyncedAt: now,
    leaseRenewedAt: now,
    recordAddresses: probe.reachable,
    unreachable: probe.unreachable,
    updatedAt: now,
  }).where(ofOrganization);
  return true;
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
 * (a 429 included) keeps the current certificate; the next sync tries again.
 */
export const ensureClusterDomainCertificate = Effect.fn("ClusterDomain.ensureCertificate")(function* (organizationId: string) {
  const current = yield* loadClusterDomain(organizationId);
  if (current?.certificateNotAfter && current.certificateNotAfter.getTime() - Date.now() > CERTIFICATE_RENEW_BEFORE_MS) return false;
  const issued = yield* withClusterDomain(organizationId, ({ endpoint, name, token }) => {
    const { privateKeyPem, csrPem } = createWildcardCsr(name);
    return requestHostedDomainCertificate({ endpoint, name, token, csr: csrPem }).pipe(
      Effect.flatMap((chain) => Effect.try({
        // X509Certificate reads the first certificate of the chain: the leaf.
        try: () => ({ name, chain, privateKeyPem, notAfter: new Date(new X509Certificate(chain).validTo) }),
        catch: (cause) => new HostedDnsError({ operation: "request certificate", cause }),
      })),
    );
  }).pipe(Effect.catchTag("HostedDnsError", (error) =>
    Effect.logWarning("Wildcard certificate issuance failed; the current certificate stays.", error).pipe(Effect.as(null))));
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
