/** @data-source remote: Servers waiting for a passkey to approve their join, polled every 5s while the Servers page is open, and the passkey an install line carries, read fresh each time an install command is shown */
import { queryOptions } from "@tanstack/react-query";
import { pendingPasskeyJoinsServerFn } from "#/modules/machines/waiting/passkey-join.functions";
import { WAITING_POLL_MS } from "#/modules/machines/waiting/waiting.queries";
import { installKeyServerFn } from "#/modules/passkeys/passkeys.functions";

export const pendingJoinsKey = (organizationSlug: string) => ["pending-joins", organizationSlug] as const;

export function pendingJoinsQueryOptions(organizationSlug: string) {
  return queryOptions({
    queryKey: pendingJoinsKey(organizationSlug),
    queryFn: () => pendingPasskeyJoinsServerFn({ data: { organizationSlug } }),
    staleTime: WAITING_POLL_MS,
    refetchInterval: WAITING_POLL_MS,
  });
}

export function installKeyQueryOptions(organizationSlug: string) {
  return queryOptions({
    queryKey: ["install-key", organizationSlug] as const,
    queryFn: () => installKeyServerFn({ data: { organizationSlug } }),
    staleTime: 0,
  });
}
