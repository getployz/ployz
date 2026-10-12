import { useEffect, useState } from "react";
import { useQueryClient } from "@tanstack/react-query";
import { toast } from "sonner";
import type { EnvironmentRef } from "@ployz/sdk";
import { toErrorMessage } from "#/lib/error-message";
import { volumeRunKeys } from "#/modules/volume-run/volume-run.queries";
import { refreshMirrorServerFn } from "./volume-copies.functions";
import { useLastMirrorRefresh } from "./volume-copies.queries";

/** Refresh now for one Volume's mirror, and its newest finished rounds for the model's `lastSync` and `lastDone`. */
export function useRefreshMirror(organizationSlug: string, environment: EnvironmentRef, volumeId: string) {
  const queryClient = useQueryClient();
  const [sending, setSending] = useState(false);
  const [watching, setWatching] = useState<string | null>(null);
  const { ended: lastSync, done: lastDone } = useLastMirrorRefresh(organizationSlug, volumeId, watching !== null);

  useEffect(() => {
    if (watching === null || lastSync?.id !== watching) return;
    setWatching(null);
    void queryClient.invalidateQueries({ queryKey: volumeRunKeys.all });
    if (lastSync.state === "done") toast.success("Mirror refreshed");
    else toast.error(lastSync.message ?? "Refresh failed");
  }, [watching, lastSync, queryClient]);

  const refresh = () => {
    setSending(true);
    refreshMirrorServerFn({ data: { organizationSlug, volumeId, environment } }).then((result) => {
      if (!result.ok) {
        toast.error(result.refusal.message);
        return;
      }
      setWatching(result.run.id);
      void queryClient.invalidateQueries({ queryKey: volumeRunKeys.all });
    }, (error: Error) => toast.error(toErrorMessage(error, "Could not refresh the mirror")))
      .finally(() => setSending(false));
  };
  return { refresh, pending: sending || watching !== null, lastSync, lastDone };
}
