import { createOptimisticAction, useLiveSuspenseQuery } from "@tanstack/react-db";
import { useLoaderData } from "@tanstack/react-router";
import { toast } from "sonner";
import { getClusterNameCollection, getOrganizationEnrollmentCollection } from "#/collections/collections";
import { useCollectionScope } from "#/collections/use-collection-scope";
import { organizationEnrollmentStatus } from "#/modules/machines/enrollment";
import { clusterNameView } from "./cluster-name";
import { renameClusterServerFn } from "./cluster-name.functions";

/** The Organization's Cluster Name. A rename shows at once, saves in the background, and rolls back with a toast. */
export function useClusterName(organizationSlug: string) {
  const scope = useCollectionScope();
  const collection = getClusterNameCollection(organizationSlug, scope);
  const { data: rows } = useLiveSuspenseQuery(collection);
  const { data: enrollment } = useLiveSuspenseQuery(getOrganizationEnrollmentCollection(organizationSlug, scope));
  const { organizationId } = useLoaderData({ from: "/_protected/cloud/$organizationSlug" });
  const view = clusterNameView(rows[0], organizationSlug, organizationEnrollmentStatus(enrollment[0]) === "ready");
  const rename = createOptimisticAction<string>({
    onMutate: (name) => {
      if (collection.has(organizationId)) collection.update(organizationId, (row) => { Object.assign(row, { name, refusal: null }); });
      else collection.insert({ id: organizationId, name, appliedName: null, refusal: null });
    },
    mutationFn: (name) => renameClusterServerFn({ data: { organizationSlug, name } })
      .then((row) => collection.utils.writeUpsert(row)),
  });
  return {
    view,
    rename: (name: string) => {
      rename(name).when("settled").catch(() => toast.error("Couldn't rename the cluster."));
    },
  };
}
