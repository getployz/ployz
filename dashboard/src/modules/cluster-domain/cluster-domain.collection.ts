import type { ClusterDomainRow } from "./cluster-domain";
import { defineOrgCollection } from "#/collections/change-log/declare";

export const clusterDomain = defineOrgCollection({
  name: "organization_cluster_domain",
  sources: ["organization_cluster_domain"],
  key: (row: ClusterDomainRow) => row.id,
});
