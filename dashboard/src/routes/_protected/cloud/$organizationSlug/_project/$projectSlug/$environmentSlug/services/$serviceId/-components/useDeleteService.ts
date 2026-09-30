import { useNavigate, useParams } from "@tanstack/react-router";
import type { EnvironmentRef, ServiceListing } from "@ployz/sdk";
import { useStoreWriter } from "#/modules/config-store/store-write";
import { useCachedStoreView, volumesQuery } from "#/modules/config-store/store-view.queries";
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
 * Volume, mounted by it alone, goes with it; the Deploy asks before deleting its data.
 */
export function useRemoveStoreService(environment: EnvironmentRef, service: ServiceListing) {
  const { params, close } = useCloseService();
  const writer = useStoreWriter(params.organizationSlug);
  const volumes = useCachedStoreView(params.organizationSlug, service.template ? volumesQuery(environment) : null);
  const own = volumes?.ok ? volumes.value.volumes.filter((volume) =>
    volume.mounts.length > 0 && volume.mounts.every((mount) => mount.service === service.name)) : [];

  return function removeService() {
    writer.commit({ command: "remove_service", environment, service: service.name });
    for (const volume of own) writer.commit({ command: "remove_volume", environment, volume: volume.name });
    close();
  };
}
