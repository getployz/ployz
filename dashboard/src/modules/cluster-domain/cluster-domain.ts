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
 * Whether the Cluster Domain serves traffic. Only problems the user can fix, or that break their
 * traffic, surface; the rest are Ployz's to fix and read as ready or setting up.
 */
export type ClusterDomainState =
  | { readonly kind: "setting_up" }
  | { readonly kind: "ready" }
  | { readonly kind: "attention"; readonly reason: "no_servers" | "no_public_ip" | "https_down" }
  | { readonly kind: "attention"; readonly reason: "port_80" | "port_80_taken"; readonly addresses: readonly string[] }
  | { readonly kind: "attention"; readonly reason: "ingress_down"; readonly servers: readonly string[] };

/** An ingress Server left out of the Cluster Domain's records while other Servers carry its traffic. */
export type ExcludedServer =
  | { readonly address: string; readonly reason: "port_80" | "port_80_taken" }
  | { readonly server: string; readonly reason: "ingress_down" };

/** What the user sees of the Cluster Domain: its state, and the Servers it does without. */
export type ClusterDomainStatus = { readonly state: ClusterDomainState; readonly excluded: readonly ExcludedServer[] };

export function clusterDomainStatus(
  domain: Pick<ClusterDomainRow, "recordsSyncedAt" | "traffic" | "certificateNotAfter">,
  now: Date,
): ClusterDomainStatus {
  const { traffic } = domain;
  const probed = traffic?.kind === "probed" ? traffic : null;
  const closed = (probed?.unreachable ?? []).map((server) => server.address);
  const taken = (probed?.taken ?? []).map((server) => server.address);
  const down = (probed?.down ?? []).map((server) => server.name);
  // A row checked before `reachable` was stored can't say whether any Server answered, so it reads as none did.
  if (probed?.reachable?.length) {
    return {
      state: clusterDomainState(domain, now),
      excluded: [
        ...closed.map((address) => ({ address, reason: "port_80" as const })),
        ...taken.map((address) => ({ address, reason: "port_80_taken" as const })),
        ...down.map((server) => ({ server, reason: "ingress_down" as const })),
      ],
    };
  }
  const state: ClusterDomainState = traffic?.kind === "no_servers" || traffic?.kind === "no_public_ip"
    ? { kind: "attention", reason: traffic.kind }
    : taken.length > 0
    ? { kind: "attention", reason: "port_80_taken", addresses: taken }
    : down.length > 0
    ? { kind: "attention", reason: "ingress_down", servers: down }
    : closed.length > 0
    ? { kind: "attention", reason: "port_80", addresses: closed }
    : clusterDomainState(domain, now);
  return { state, excluded: [] };
}

/** The state once some Server carries traffic: what Ployz is still setting up or fixing. */
function clusterDomainState(
  domain: Pick<ClusterDomainRow, "recordsSyncedAt" | "certificateNotAfter">,
  now: Date,
): ClusterDomainState {
  if (domain.certificateNotAfter !== null && domain.certificateNotAfter <= now) return { kind: "attention", reason: "https_down" };
  if (domain.recordsSyncedAt === null || domain.certificateNotAfter === null) return { kind: "setting_up" };
  return { kind: "ready" };
}
