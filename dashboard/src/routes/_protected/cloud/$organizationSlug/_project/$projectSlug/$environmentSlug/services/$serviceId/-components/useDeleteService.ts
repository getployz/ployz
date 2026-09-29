import { useNavigate, useParams } from "@tanstack/react-router";
import type { EnvironmentRef } from "@ployz/sdk";
import { useServiceWriter } from "#/modules/services/services.collection";
import { deleteService } from "#/modules/services/delete-service";
import { useStoreWriter } from "#/modules/config-store/store-write";
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

export function useDeleteService(serviceId: string) {
  const { params, close } = useCloseService();
  const collection = useServiceWriter(params.organizationSlug);

  return function removeService() {
    deleteService(collection, serviceId);
    close();
  };
}

/** Stages a Store Service's removal: a Deploy removes what runs; until then Discard brings it back. */
export function useRemoveStoreService(environment: EnvironmentRef, service: string) {
  const { params, close } = useCloseService();
  const writer = useStoreWriter(params.organizationSlug);

  return function removeService() {
    writer.commit({ command: "remove_service", environment, service });
    close();
  };
}
