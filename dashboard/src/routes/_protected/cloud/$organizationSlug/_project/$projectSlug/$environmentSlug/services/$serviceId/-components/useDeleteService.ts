import { useNavigate, useParams } from "@tanstack/react-router";
import { toast } from "sonner";
import { useServiceWriter } from "#/modules/services/services.collection";
import { deleteService } from "#/modules/services/delete-service";
import {
  ENVIRONMENT_INDEX_ROUTE_TO,
  ENVIRONMENT_ROUTE_FROM,
} from "../../../-components/environment-route-paths";

export function useDeleteService(serviceId: string) {
  const params = useParams({ from: ENVIRONMENT_ROUTE_FROM });
  const navigate = useNavigate();
  const collection = useServiceWriter(params.organizationSlug);

  return async function removeService() {
    try {
      await deleteService(collection, serviceId);
      await navigate({
        to: ENVIRONMENT_INDEX_ROUTE_TO,
        params,
        replace: true,
        search: (prev) => prev,
      });
    } catch (error) {
      toast.error(error instanceof Error ? error.message : "Failed to delete service.");
    }
  };
}
