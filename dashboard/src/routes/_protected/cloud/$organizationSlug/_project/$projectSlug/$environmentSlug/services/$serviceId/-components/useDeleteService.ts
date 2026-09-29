import { useNavigate, useParams } from "@tanstack/react-router";
import type { EnvironmentRef } from "@ployz/sdk";
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

/** Stages a Store Service's removal: a Deploy removes what runs; until then Discard brings it back. */
export function useRemoveStoreService(environment: EnvironmentRef, service: string) {
  const { params, close } = useCloseService();
  const writer = useStoreWriter(params.organizationSlug);

  return function removeService() {
    writer.commit({ command: "remove_service", environment, service });
    close();
  };
}
