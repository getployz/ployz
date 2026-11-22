import { useLoaderData, useNavigate } from "@tanstack/react-router";
import { usePlaceNewNode } from "./useCanvasPositionMutation";
import { createConfigCommand } from "#/modules/config-store/store-configs";
import { useStoreWriter } from "#/modules/config-store/store-write";
import { ENVIRONMENT_RESOURCE_ROUTE_TO, ENVIRONMENT_ROUTE_FROM, type EnvironmentRouteParams } from "#/lib/environment-routes";
import type { FlowPosition } from "./types";

/** Creates a Config where the user asked, then opens it so its first file can be added. */
export function useConfigCreator(params: EnvironmentRouteParams, environmentId: string) {
  const place = usePlaceNewNode(params.organizationSlug);
  const writer = useStoreWriter(params.organizationSlug);
  const navigate = useNavigate();
  const { store } = useLoaderData({ from: ENVIRONMENT_ROUTE_FROM });
  return async (name: string, position: FlowPosition) => {
    const id = crypto.randomUUID();
    place({ environmentId, resourceType: "config", resourceId: id, ...position });
    await writer.commit(createConfigCommand(id, store, name));
    await navigate({ to: ENVIRONMENT_RESOURCE_ROUTE_TO, params: { ...params, resourceId: id }, search: (prev) => prev });
  };
}
