import { Schema } from "effect";
import type { OrganizationClusterDomain } from "#/modules/cluster-domain/tables";

/** The Org Store's view of the Cluster Domain row, keyed by Organization id. Tokens and keys never leave the server. */
export type ClusterDomainRow = { id: string } & Pick<
  OrganizationClusterDomain,
  "name" | "recordsSyncedAt" | "traffic" | "certificateNotAfter" | "checkedAt"
>;

export const CheckClusterDomainInput = Schema.Struct({
  organizationSlug: Schema.String.check(Schema.isTrimmed(), Schema.isNonEmpty()),
});

/**
 * What the user sees of the Cluster Domain. Only problems the user can fix, or that break their
 * traffic, surface; the rest are Ployz's to fix and read as ready or setting up.
 */
export type ClusterDomainStatus =
  | { readonly kind: "setting_up" }
  | { readonly kind: "ready" }
  | { readonly kind: "attention"; readonly reason: "no_servers" | "no_public_ip" | "https_down" }
  | { readonly kind: "attention"; readonly reason: "port_80"; readonly addresses: readonly string[] };

export function clusterDomainStatus(
  domain: Pick<ClusterDomainRow, "recordsSyncedAt" | "traffic" | "certificateNotAfter">,
  now: Date,
): ClusterDomainStatus {
  const { traffic } = domain;
  if (traffic?.kind === "no_servers" || traffic?.kind === "no_public_ip") return { kind: "attention", reason: traffic.kind };
  if (traffic?.kind === "probed" && traffic.unreachable.length > 0) {
    return { kind: "attention", reason: "port_80", addresses: traffic.unreachable.map((server) => server.address) };
  }
  if (domain.certificateNotAfter !== null && domain.certificateNotAfter <= now) return { kind: "attention", reason: "https_down" };
  if (domain.recordsSyncedAt === null || domain.certificateNotAfter === null) return { kind: "setting_up" };
  return { kind: "ready" };
}
