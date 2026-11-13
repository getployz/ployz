import { queryOptions } from "@tanstack/react-query";
import { listPasskeysServerFn, serversWithoutPasskeyServerFn } from "#/modules/passkeys/passkeys.functions";

export type PasskeysView = Awaited<ReturnType<typeof listPasskeysServerFn>>;
export type PasskeyRow = PasskeysView["passkeys"][number];
export type ServerTrust = PasskeyRow["servers"][number];

/** Whether every Server Cloud reads lists the passkey. None read, none trusted. */
export const trustedEverywhere = (passkey: PasskeyRow) =>
  passkey.servers.length > 0 && passkey.servers.every((server) => server.approval !== null && server.approval !== "unanswered");

const WAITING_POLL_MS = 5_000;

/**
 * Your passkeys in this Organization and what a Server says of each. Approval happens on a Server's terminal, where no
 * Cloud write announces it, so the list asks again every few seconds while a Server still lacks one.
 */
export function passkeysQueryOptions(organizationSlug: string) {
  return queryOptions({
    queryKey: ["passkeys", organizationSlug],
    queryFn: ({ signal }) => listPasskeysServerFn({ data: { organizationSlug }, signal }),
    staleTime: 0,
    refetchInterval: (query) =>
      query.state.data?.passkeys.some((passkey) => !trustedEverywhere(passkey)) ? WAITING_POLL_MS : false,
  });
}

/**
 * The IDs of the Servers that trust no passkey yet. Approval happens on a Server's terminal, so the list asks again
 * every few seconds while one is waiting.
 */
export function serversWithoutPasskeyQueryOptions(organizationSlug: string) {
  return queryOptions({
    queryKey: ["passkeys", organizationSlug, "servers-without"],
    queryFn: ({ signal }) => serversWithoutPasskeyServerFn({ data: { organizationSlug }, signal }),
    staleTime: 0,
    refetchInterval: (query) => (query.state.data?.length ?? 0) > 0 ? WAITING_POLL_MS : false,
  });
}
