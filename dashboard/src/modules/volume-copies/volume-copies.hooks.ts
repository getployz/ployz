import { useQueryClient } from "@tanstack/react-query";
import { useState } from "react";
import { toast } from "sonner";
import type { EnvironmentRef } from "@ployz/sdk";
import { toErrorMessage } from "#/lib/error-message";
import { volumeRunKeys } from "#/modules/volume-run/volume-run.queries";
import { refreshMirrorServerFn } from "./volume-copies.functions";
import { useMirrorStatuses } from "./volume-copies.queries";

/** Refresh now for one Volume's mirror, and what the mirror syncs last saw of each mirror. */
export function useRefreshMirror(organizationSlug: string, environment: EnvironmentRef, volumeId: string) {
  const [sending, setSending] = useState(false);
  const queryClient = useQueryClient();
  const statuses = useMirrorStatuses(organizationSlug, volumeId);

  const refresh = () => {
    setSending(true);
    refreshMirrorServerFn({ data: { organizationSlug, volumeId, environment } }).then((result) => {
      if (result.ok) toast.success(result.lines.join("\n"));
      else toast.error(result.refusal.message);
      void queryClient.invalidateQueries({ queryKey: volumeRunKeys.all });
    }, (error: Error) => toast.error(toErrorMessage(error, "Could not refresh the mirror")))
      .finally(() => setSending(false));
  };
  return { refresh, pending: sending, statuses };
}
