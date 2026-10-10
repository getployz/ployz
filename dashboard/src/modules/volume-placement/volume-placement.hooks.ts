import { useState } from "react";
import { useQueryClient } from "@tanstack/react-query";
import { toast } from "sonner";
import type { EnvironmentRef, MachineId } from "@ployz/sdk";
import { toErrorMessage } from "#/lib/error-message";
import type { Placement } from "./volume-placement";
import { discardPlacementServerFn, stagePlacementServerFn } from "./volume-placement.functions";
import { volumePlacementKeys } from "./volume-placement.queries";

type PlacementResult = { readonly ok: true } | { readonly ok: false; readonly refusal: { readonly message: string } };

/** Stage or discard a Volume's placement. A refusal toasts; the row then arrives on the change stream. */
export function useStagePlacement(organizationSlug: string, environment: EnvironmentRef, volume: { readonly id: string; readonly name: string }) {
  const queryClient = useQueryClient();
  const [pending, setPending] = useState(false);
  const settle = (write: Promise<PlacementResult>) => {
    setPending(true);
    write.then((result) => {
      if (!result.ok) toast.error(result.refusal.message);
      void queryClient.invalidateQueries({ queryKey: volumePlacementKeys.all });
    }, (error: Error) => toast.error(toErrorMessage(error, `Could not change ${volume.name}`)))
      .finally(() => setPending(false));
  };
  const stage = (staged: Placement | null, mirrors: readonly MachineId[]) =>
    settle(stagePlacementServerFn({ data: { organizationSlug, volumeId: volume.id, environment, staged, mirrors: [...mirrors] } }));
  const discard = () => settle(discardPlacementServerFn({ data: { organizationSlug, volumeId: volume.id } }));
  return { stage, discard, pending };
}
