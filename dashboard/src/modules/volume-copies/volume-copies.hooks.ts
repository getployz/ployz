import { useState } from "react";
import { toast } from "sonner";
import type { EnvironmentRef } from "@ployz/sdk";
import { toErrorMessage } from "#/lib/error-message";
import { useWatchVolumeRun } from "#/modules/volume-run/volume-run.queries";
import { refreshMirrorServerFn } from "./volume-copies.functions";
import { useLastMirrorRefresh } from "./volume-copies.queries";

/** Refresh now for one Volume's mirror, and its newest finished rounds for the model's `lastSync` and `lastDone`. */
export function useRefreshMirror(organizationSlug: string, environment: EnvironmentRef, volumeId: string) {
  const [sending, setSending] = useState(false);
  const { ended: lastSync, done: lastDone } = useLastMirrorRefresh(organizationSlug, volumeId);
  const { watch, watching } = useWatchVolumeRun(organizationSlug, (run) => {
    if (run?.state === "done") toast.success("Mirror refreshed");
    else toast.error(run?.message ?? "Refresh failed");
  });

  const refresh = () => {
    setSending(true);
    refreshMirrorServerFn({ data: { organizationSlug, volumeId, environment } }).then((result) => {
      if (result.ok) watch(result.run.id);
      else toast.error(result.refusal.message);
    }, (error: Error) => toast.error(toErrorMessage(error, "Could not refresh the mirror")))
      .finally(() => setSending(false));
  };
  return { refresh, pending: sending || watching, lastSync, lastDone };
}
