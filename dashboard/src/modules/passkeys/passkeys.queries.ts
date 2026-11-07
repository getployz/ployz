import { queryOptions } from "@tanstack/react-query";
import { listPasskeysServerFn } from "#/modules/passkeys/passkeys.functions";

export type PasskeysView = Awaited<ReturnType<typeof listPasskeysServerFn>>;
export type PasskeyRow = PasskeysView["passkeys"][number];

const WAITING_POLL_MS = 5_000;

/**
 * Your passkeys in this Organization and what a Server says of each. Approval happens on a Server's terminal, where no
 * Cloud write announces it, so the list asks again every few seconds while one waits.
 */
export function passkeysQueryOptions(organizationSlug: string) {
  return queryOptions({
    queryKey: ["passkeys", organizationSlug],
    queryFn: ({ signal }) => listPasskeysServerFn({ data: { organizationSlug }, signal }),
    staleTime: 0,
    refetchInterval: (query) =>
      query.state.data?.passkeys.some((passkey) => passkey.approval === null) ? WAITING_POLL_MS : false,
  });
}
