import { useLoaderData, useParams } from "@tanstack/react-router";
import { useStorePendingName, useStorePendingNodes, type NodeKind } from "#/modules/config-store/store-pending";
import { ENVIRONMENT_ROUTE_FROM } from "../environment-route-paths";

/** A canvas node's name under a pending rename, and whether a pending removal is taking it away. */
export function usePendingNode(node: NodeKind, storeName: string) {
  const { organizationSlug } = useParams({ from: ENVIRONMENT_ROUTE_FROM });
  const { store } = useLoaderData({ from: ENVIRONMENT_ROUTE_FROM });
  const name = useStorePendingName(organizationSlug, store, node, storeName);
  const leaving = useStorePendingNodes(organizationSlug, store).leaving[node].has(storeName);
  return { name, leaving };
}
