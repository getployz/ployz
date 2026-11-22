import type { VolumeKind } from "@ployz/sdk";
import { useLoaderData } from "@tanstack/react-router";
import { usePlaceNewNode } from "./useCanvasPositionMutation";
import { createVolumeCommand } from "#/modules/config-store/store-volumes";
import { useStoreWriter } from "#/modules/config-store/store-write";
import { ENVIRONMENT_ROUTE_FROM } from "#/lib/environment-routes";
import type { FlowPosition } from "./types";

/** On the canvas at once, saved in the background; a refused create goes again with a toast. */
export function useVolumeCreator(organizationSlug: string, environmentId: string) {
  const place = usePlaceNewNode(organizationSlug);
  const writer = useStoreWriter(organizationSlug);
  const { store } = useLoaderData({ from: ENVIRONMENT_ROUTE_FROM });
  return (input: { name: string; storage: VolumeKind; position: FlowPosition }) => {
    const id = crypto.randomUUID();
    place({ environmentId, resourceType: "volume", resourceId: id, ...input.position });
    writer.commit(createVolumeCommand(id, store, input.name, input.storage));
  };
}
