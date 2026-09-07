"use client";

import { useNavigate, useParams } from "@tanstack/react-router";
import { Trash2Icon } from "lucide-react";
import { toast } from "sonner";
import { Button } from "#/components/ui/button";
import {
  ENVIRONMENT_INDEX_ROUTE_TO,
  ENVIRONMENT_ROUTE_FROM,
} from "#/routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/-components/environment-route-paths";
import type { ServiceDrawerState } from "#/routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/services/$serviceId/-components/useServiceDrawerState";

export function ServiceDangerSection({
  state,
}: {
  state: ServiceDrawerState;
}) {
  const params = useParams({ from: ENVIRONMENT_ROUTE_FROM });
  const navigate = useNavigate();

  async function deleteService() {
    try {
      const transaction = state.collection.update(state.service.id, (draft) => {
        draft.deletedAt = new Date();
      });
      await transaction.isPersisted.promise;
      await navigate({
        to: ENVIRONMENT_INDEX_ROUTE_TO,
        params: {
          organizationSlug: params.organizationSlug,
          projectSlug: params.projectSlug,
          environmentSlug: params.environmentSlug,
        },
        replace: true,
        search: (prev) => prev,
      });
    } catch (error) {
      toast.error(
        error instanceof Error
          ? error.message
          : "Failed to delete service.",
      );
      throw error;
    }
  }

  return (
    <div className="flex flex-col items-start justify-between gap-4 rounded-xl border border-destructive-border bg-destructive-soft p-4 sm:flex-row sm:items-center">
      <div className="min-w-0">
        <div className="text-sm font-semibold text-destructive">
          Delete this service
        </div>
        <p className="mt-1 text-sm text-destructive/85">
          Permanently deletes all deployments and removes it from this
          environment. This cannot be undone.
        </p>
      </div>
      <Button
        variant="destructive"
        className="shrink-0"
        onClick={() => void deleteService()}
      >
        <Trash2Icon data-icon="inline-start" />
        Delete service
      </Button>
    </div>
  );
}
