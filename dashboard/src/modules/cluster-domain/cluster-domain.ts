import { Schema } from "effect";
import type { OrganizationClusterDomain } from "#/modules/cluster-domain/tables";

/** The Org Store's view of the Cluster Domain row, keyed by Organization id. Tokens and keys never leave the server. */
export type ClusterDomainRow = { id: string } & Pick<
  OrganizationClusterDomain,
  "name" | "reservedAt" | "leaseRenewedAt" | "recordsSyncedAt" | "recordAddresses" | "unreachable" | "certificateNotAfter"
>;

export const PublishClusterDomainInput = Schema.Struct({
  organizationSlug: Schema.String.check(Schema.isTrimmed(), Schema.isNonEmpty()),
});
