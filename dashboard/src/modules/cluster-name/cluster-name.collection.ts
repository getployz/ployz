import type { ClusterNameRow } from "./cluster-name";
import { defineOrgCollection } from "#/collections/change-log/declare";

/** The Organization's Cluster Name as Cloud keeps it: no row reads as the default, or unset on a Cluster founded before names. */
export const clusterName = defineOrgCollection({
  name: "organization_cluster_name",
  sources: ["organization_cluster_name"],
  key: (row: ClusterNameRow) => row.id,
});
