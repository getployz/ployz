import type { ClusterDomainStatus } from "#/modules/cluster-domain/cluster-domain";
import type { RuntimeCertificateRecord } from "#/modules/runtime/runtime.collection";

/**
 * What a user sees of one public domain on a Service. Generated domains follow the Cluster Domain;
 * custom domains follow their own certificate row, whose refusals name the DNS problem.
 */
export type PublicDomainStatus =
  | { readonly kind: "live"; readonly viaProxy?: true }
  | { readonly kind: "not_deployed" }
  | { readonly kind: "setting_up" }
  | { readonly kind: "issuing" }
  | { readonly kind: "needs_dns" }
  | { readonly kind: "dns_elsewhere" }
  | { readonly kind: "port_closed" }
  | { readonly kind: "redirects_to_https" }
  | { readonly kind: "cert_failed"; readonly retryAt: Date | null }
  | { readonly kind: "unreachable" }
  | { readonly kind: "https_down" }
  | { readonly kind: "unknown" };

export function publicDomainStatus(input: {
  /** A generated domain under the Cluster Domain, or a custom domain (Route) with its certificate row. */
  readonly domain: { readonly kind: "generated" } | { readonly kind: "custom"; readonly certificate: RuntimeCertificateRecord | null };
  /** Whether the hostname is in the Service's Applied State. */
  readonly deployed: boolean;
  /** Whether the Runtime Watch is observing the Cluster, so a missing certificate row means none yet. */
  readonly observed: boolean;
  /** Null while the Organization holds no Cluster Domain. */
  readonly clusterDomain: ClusterDomainStatus | null;
}): PublicDomainStatus {
  const { clusterDomain, domain } = input;
  if (!input.deployed) return { kind: "not_deployed" };
  // Every domain lands on the same ingress Servers.
  if (clusterDomain?.kind === "attention" && clusterDomain.reason !== "https_down") return { kind: "unreachable" };
  if (domain.kind === "generated") {
    if (clusterDomain?.kind === "attention") return { kind: "https_down" };
    return clusterDomain?.kind === "ready" ? { kind: "live" } : { kind: "setting_up" };
  }
  const { certificate } = domain;
  if (!input.observed) return { kind: "unknown" };
  if (certificate === null || certificate.status === "pending") return { kind: "issuing" };
  if (certificate.status === "available") return certificate.viaProxy ? { kind: "live", viaProxy: true } : { kind: "live" };
  if (certificate.status !== "failure") return { kind: "unknown" };
  switch (certificate.backoff?.failureKind) {
    case "does_not_resolve":
      return { kind: "needs_dns" };
    case "reaches_elsewhere":
      return { kind: "dns_elsewhere" };
    case "unreachable":
      return { kind: "port_closed" };
    case "redirects_to_https":
      return { kind: "redirects_to_https" };
  }
  return { kind: "cert_failed", retryAt: certificate.backoff ? new Date(certificate.backoff.nextAttemptAt) : null };
}

/**
 * A custom hostname's certificate row: its own, else the published `*.<parent>` wildcard's, which
 * serves every hostname one label under it and leaves the Engine keeping no row of its own.
 */
export function certificateFor(
  hostname: string,
  certificates: readonly RuntimeCertificateRecord[],
): RuntimeCertificateRecord | null {
  const wildcard = `*.${hostname.slice(hostname.indexOf(".") + 1)}`;
  return certificates.find((row) => row.hostname === hostname) ?? certificates.find((row) => row.hostname === wildcard) ?? null;
}

export type DnsRecord = { readonly type: "CNAME" | "A" | "AAAA"; readonly name: string; readonly value: string };

/**
 * The records that point a custom domain here: a CNAME to the Cluster Domain, which follows the
 * ingress Servers, or A/AAAA records to the ingress addresses at an apex or with no Cluster Domain.
 */
export function dnsRecordsFor(hostname: string, clusterDomain: string | null, ingressAddresses: readonly string[]): DnsRecord[] {
  // ponytail: the registrable domain is taken as the last two labels; add a public-suffix list when a
  // multi-label suffix (example.co.uk) shows up.
  const labels = hostname.split(".");
  const name = labels.length > 2 ? labels.slice(0, -2).join(".") : "@";
  if (clusterDomain !== null && name !== "@") return [{ type: "CNAME", name, value: clusterDomain }];
  return ingressAddresses.map((value) => ({ type: value.includes(":") ? "AAAA" : "A", name, value }));
}
