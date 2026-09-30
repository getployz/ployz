import { useNavigate, useParams } from "@tanstack/react-router";
import type { EnvironmentRef, ServiceListing } from "@ployz/sdk";
import { toast } from "sonner";
import { useCollectionScope } from "#/collections/use-collection-scope";
import { useStoreWriter } from "#/modules/config-store/store-write";
import { requireView, storeViewOptions, volumesQuery } from "#/modules/config-store/store-view.queries";
import {
  ENVIRONMENT_INDEX_ROUTE_TO,
  ENVIRONMENT_ROUTE_FROM,
} from "../../../-components/environment-route-paths";

function useCloseService() {
  const params = useParams({ from: ENVIRONMENT_ROUTE_FROM });
  const navigate = useNavigate();
  return {
    params,
    close: () => void navigate({ to: ENVIRONMENT_INDEX_ROUTE_TO, params, replace: true, search: (prev) => prev }),
  };
}

/**
 * Stages a Store Service's removal: a Deploy removes what runs; until then Discard brings it back. A Database Preset's
 * Volume, mounted by it alone, goes with it once the Service's removal is accepted; the Deploy asks before deleting
 * its data.
 */
export function useRemoveStoreService(environment: EnvironmentRef, service: ServiceListing) {
  const { params, close } = useCloseService();
  const writer = useStoreWriter(params.organizationSlug);
  const scope = useCollectionScope();

  // The preset's own Volumes, as mounted before its removal; read from the Store on a cold cache.
  async function ownVolumes() {
    if (!service.template) return [];
    const view = requireView(await scope.queryClient.ensureQueryData(
      storeViewOptions(params.organizationSlug, scope, volumesQuery(environment))));
    return view.volumes.filter((volume) =>
      volume.mounts.length > 0 && volume.mounts.every((mount) => mount.service === service.name));
  }

  async function removeService() {
    const own = await ownVolumes();
    const removed = writer.commit({ command: "remove_service", environment, service: service.name });
    close();
    if (own.length === 0) return;
    // A refused Service removal has toasted and rolled back; its Volumes stay.
    if (!await removed.isPersisted.promise.then(() => true, () => false)) return;
    for (const volume of own) writer.commit({ command: "remove_volume", environment, volume: volume.name });
  }

  return () => void removeService().catch((error: Error) => toast.error(error.message));
}
