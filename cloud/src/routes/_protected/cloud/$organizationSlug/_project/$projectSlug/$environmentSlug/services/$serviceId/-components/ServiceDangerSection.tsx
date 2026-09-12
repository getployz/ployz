"use client";

import { Trash2Icon } from "lucide-react";
import { Button } from "#/components/ui/button";
import { useDeleteService } from "./useDeleteService";
import type { ServiceDrawerState } from "#/routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/services/$serviceId/-components/useServiceDrawerState";

export function ServiceDangerSection({
  state,
}: {
  state: ServiceDrawerState;
}) {
  const deleteService = useDeleteService(state.service.id);

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
