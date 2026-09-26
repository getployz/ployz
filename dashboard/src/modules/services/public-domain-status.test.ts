import { describe, expect, it } from "vitest";
import type { ClusterDomainStatus } from "#/modules/cluster-domain/cluster-domain";
import type { RuntimeCertificateRecord } from "#/modules/runtime/runtime.collection";
import { certificateFor, dnsRecordsFor, publicDomainStatus } from "#/modules/services/public-domain-status";

const ready: ClusterDomainStatus = { kind: "ready" };
const certificate = (status: string, failureKind?: string): RuntimeCertificateRecord => ({
  hostname: "www.acme.com",
  status,
  lastError: failureKind ? "raw ACME text" : null,
  backoff: failureKind ? { failureKind, nextAttemptAt: "2026-09-25T12:12:00Z", failures: 2 } : null,
  viaProxy: false,
});

const generated = { kind: "generated" } as const;
const custom = (row: RuntimeCertificateRecord | null) => ({ kind: "custom", certificate: row }) as const;

describe("publicDomainStatus", () => {
  it.each([
    ["an undeployed domain waits for the next deploy",
      { domain: custom(certificate("available")), deployed: false, observed: true, clusterDomain: ready }, { kind: "not_deployed" }],
    ["servers that can't receive traffic affect every domain",
      { domain: custom(certificate("available")), deployed: true, observed: true, clusterDomain: { kind: "attention", reason: "port_80", addresses: ["203.0.113.1"] } },
      { kind: "unreachable" }],
    ["no servers affect every domain",
      { domain: generated, deployed: true, observed: true, clusterDomain: { kind: "attention", reason: "no_servers" } }, { kind: "unreachable" }],
    ["a generated domain is live once the Cluster Domain is ready",
      { domain: generated, deployed: true, observed: true, clusterDomain: ready }, { kind: "live" }],
    ["a generated domain doesn't wait on the Runtime Watch",
      { domain: generated, deployed: true, observed: false, clusterDomain: ready }, { kind: "live" }],
    ["a generated domain sets up with its Cluster Domain",
      { domain: generated, deployed: true, observed: true, clusterDomain: { kind: "setting_up" } }, { kind: "setting_up" }],
    ["a generated domain goes down with an expired wildcard",
      { domain: generated, deployed: true, observed: true, clusterDomain: { kind: "attention", reason: "https_down" } }, { kind: "https_down" }],
    ["an expired wildcard leaves custom domains alone",
      { domain: custom(certificate("available")), deployed: true, observed: true, clusterDomain: { kind: "attention", reason: "https_down" } }, { kind: "live" }],
    ["a custom domain with a certificate is live",
      { domain: custom(certificate("available")), deployed: true, observed: true, clusterDomain: null }, { kind: "live" }],
    ["a custom domain that doesn't resolve needs DNS",
      { domain: custom(certificate("failure", "does_not_resolve")), deployed: true, observed: true, clusterDomain: ready }, { kind: "needs_dns" }],
    ["a custom domain resolving elsewhere needs DNS pointed here",
      { domain: custom(certificate("failure", "reaches_elsewhere")), deployed: true, observed: true, clusterDomain: ready }, { kind: "dns_elsewhere" }],
    ["a custom domain with port 80 closed says so",
      { domain: custom(certificate("failure", "unreachable")), deployed: true, observed: true, clusterDomain: ready }, { kind: "port_closed" }],
    ["a proxy redirecting to HTTPS blocks the challenge",
      { domain: custom(certificate("failure", "redirects_to_https")), deployed: true, observed: true, clusterDomain: ready }, { kind: "redirects_to_https" }],
    ["a custom domain served through a proxy is live, via proxy",
      { domain: custom({ ...certificate("available"), viaProxy: true }), deployed: true, observed: true, clusterDomain: ready }, { kind: "live", viaProxy: true }],
    ["a refused certificate retries at the next attempt",
      { domain: custom(certificate("failure", "authority")), deployed: true, observed: true, clusterDomain: ready },
      { kind: "cert_failed", retryAt: new Date("2026-09-25T12:12:00Z") }],
    ["a pending certificate is issuing",
      { domain: custom(certificate("pending")), deployed: true, observed: true, clusterDomain: ready }, { kind: "issuing" }],
    ["no certificate row yet is issuing",
      { domain: custom(null), deployed: true, observed: true, clusterDomain: null }, { kind: "issuing" }],
    ["a custom domain is unknown while the Runtime Watch isn't observing",
      { domain: custom(null), deployed: true, observed: false, clusterDomain: ready }, { kind: "unknown" }],
    ["an unknown certificate status is unknown",
      { domain: custom(certificate("unknown")), deployed: true, observed: true, clusterDomain: ready }, { kind: "unknown" }],
    ["an unrecognized certificate status is unknown",
      { domain: custom(certificate("revoked")), deployed: true, observed: true, clusterDomain: ready }, { kind: "unknown" }],
  ] as const)("%s", (_, input, expected) => {
    expect(publicDomainStatus(input)).toEqual(expected);
  });
});

describe("certificateFor", () => {
  const wildcard = { ...certificate("available"), hostname: "*.acme.com" };

  it("prefers the hostname's own row", () => {
    const own = certificate("pending");
    expect(certificateFor("www.acme.com", [wildcard, own])).toBe(own);
  });

  it("falls back to the wildcard one label up", () => {
    expect(certificateFor("www.acme.com", [wildcard])).toBe(wildcard);
    expect(certificateFor("a.www.acme.com", [wildcard])).toBeNull();
    expect(certificateFor("acme.com", [wildcard])).toBeNull();
  });
});

describe("dnsRecordsFor", () => {
  const ips = ["203.0.113.1", "2001:db8::1"];

  it("points a subdomain at the Cluster Domain", () => {
    expect(dnsRecordsFor("www.acme.com", "nick.ployz.app", ips)).toEqual([{ type: "CNAME", name: "www", value: "nick.ployz.app" }]);
    expect(dnsRecordsFor("a.b.acme.com", "nick.ployz.app", ips)).toEqual([{ type: "CNAME", name: "a.b", value: "nick.ployz.app" }]);
  });

  it("points an apex at the ingress addresses, since a CNAME can't sit there", () => {
    expect(dnsRecordsFor("acme.com", "nick.ployz.app", ips)).toEqual([
      { type: "A", name: "@", value: "203.0.113.1" },
      { type: "AAAA", name: "@", value: "2001:db8::1" },
    ]);
  });

  it("points at the ingress addresses when there is no Cluster Domain", () => {
    expect(dnsRecordsFor("www.acme.com", null, ["203.0.113.1"])).toEqual([{ type: "A", name: "www", value: "203.0.113.1" }]);
  });
});
