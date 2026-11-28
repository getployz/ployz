import { toast } from "sonner";
import { useExclusiveMutation } from "#/hooks/use-exclusive-mutation";
import { toErrorMessage } from "#/lib/error-message";
import { mintMachineEnrollmentServerFn } from "#/modules/machines/enrollment.functions";
import { refusalFor } from "#/modules/organization/permissions";

export const ADD_SERVERS_REFUSED = refusalFor("servers:add", "add servers");

/** Mints one Server's enrollment token. The secret lives only in the result, so it shows once. */
export function useMintInstallLine(organizationSlug: string) {
  return useExclusiveMutation({
    mutationFn: (input: { ingressClosed: boolean }) => mintMachineEnrollmentServerFn({ data: { organizationSlug, ...input } }),
    onError: (cause) => toast.error(toErrorMessage(cause, "Couldn't make the install command.")),
  });
}
