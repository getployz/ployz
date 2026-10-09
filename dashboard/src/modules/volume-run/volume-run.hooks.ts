import { useState } from "react";
import { useQueryClient } from "@tanstack/react-query";
import { toast } from "sonner";
import type { EnvironmentRef } from "@ployz/sdk";
import { toErrorMessage } from "#/lib/error-message";
import { requestVolumeRunServerFn, type VolumeRunRequest } from "./volume-run.functions";
import { volumeRunKeys } from "./volume-run.queries";

/**
 * Starts a Volume Run from the panel. Cloud admits it or refuses in words, which toast; the run's row then arrives on the
 * change stream and the panel reads its progress from there.
 */
export function useRequestVolumeRun(organizationSlug: string, environment: EnvironmentRef, volume: { readonly id: string; readonly name: string }) {
  const queryClient = useQueryClient();
  const [pending, setPending] = useState(false);
  const request = (run: VolumeRunRequest) => {
    setPending(true);
    requestVolumeRunServerFn({ data: { organizationSlug, volumeId: volume.id, environment, run } })
      .then((result) => {
        if (!result.ok) toast.error(result.refusal.message);
        void queryClient.invalidateQueries({ queryKey: volumeRunKeys.all });
      }, (error: Error) => toast.error(toErrorMessage(error, `Could not start a run on ${volume.name}`)))
      .finally(() => setPending(false));
  };
  return { request, pending };
}
