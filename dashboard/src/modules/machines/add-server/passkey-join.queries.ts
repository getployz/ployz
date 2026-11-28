/** @data-source remote: Servers waiting for a passkey to approve their join, polled every 5s while the Servers page is open, and the passkey an install line carries, read fresh each time an install command is shown */
import { queryOptions } from "@tanstack/react-query";
import { pendingPasskeyJoinsServerFn } from "#/modules/machines/add-server/passkey-join.functions";
import { installKeyServerFn } from "#/modules/passkeys/passkeys.functions";

const PENDING_POLL_MS = 5_000;

export const pendingJoinsKey = (organizationSlug: string) => ["pending-joins", organizationSlug] as const;

export function pendingJoinsQueryOptions(organizationSlug: string) {
  return queryOptions({
    queryKey: pendingJoinsKey(organizationSlug),
    queryFn: () => pendingPasskeyJoinsServerFn({ data: { organizationSlug } }),
    staleTime: PENDING_POLL_MS,
    refetchInterval: PENDING_POLL_MS,
  });
}

export function installKeyQueryOptions(organizationSlug: string) {
  return queryOptions({
    queryKey: ["install-key", organizationSlug] as const,
    queryFn: () => installKeyServerFn({ data: { organizationSlug } }),
    staleTime: 0,
  });
}
