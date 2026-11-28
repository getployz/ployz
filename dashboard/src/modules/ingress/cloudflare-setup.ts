import { useEffect, useSyncExternalStore } from "react";
import { type QueryClient, useQuery, useQueryClient } from "@tanstack/react-query";
import { toast } from "sonner";
import { toErrorMessage } from "#/lib/error-message";
import { SERVERS_UNANSWERED_START } from "#/lib/refusal";
import { setUpCloudflareServerFn } from "#/modules/ingress/doors.functions";
import { clusterIngressKeys } from "#/modules/ingress/doors.queries";
import { clusterFoundedQueryOptions } from "#/modules/machines/enrollment.queries";

/**
 * A Cloudflare choice made before the Cluster exists. The token lives in this tab's memory only, since Cloud keeps no
 * Cloudflare credential: setup needs the Cluster's Sealing Key, so it runs with the token once the first Server joins,
 * and the token is dropped when it ends.
 */
export type HeldCloudflare =
  | { readonly kind: "waiting" | "setting-up"; readonly token: string; readonly zone: string }
  | { readonly kind: "failed"; readonly token: string; readonly zone: string; readonly message: string }
  /** `switched` once the zone is the Cluster Domain's source; until then apps keep their addresses until a switch. */
  | { readonly kind: "done"; readonly zone: string; readonly switched: boolean };

const held = new Map<string, HeldCloudflare>();
const listeners = new Set<() => void>();

const notify = () => {
  for (const listener of listeners) listener();
};

export function holdCloudflare(organizationSlug: string, next: HeldCloudflare) {
  held.set(organizationSlug, next);
  notify();
}

export function forgetCloudflare(organizationSlug: string) {
  if (held.delete(organizationSlug)) notify();
}

export function readHeldCloudflare(organizationSlug: string) {
  return held.get(organizationSlug);
}

const subscribe = (listener: () => void) => {
  listeners.add(listener);
  return () => {
    listeners.delete(listener);
  };
};

export function useHeldCloudflare(organizationSlug: string) {
  return useSyncExternalStore(subscribe, () => held.get(organizationSlug), () => undefined);
}

const ANSWER_TRIES = 12;
const ANSWER_WAIT_MS = 5_000;

/**
 * Set Cloudflare up with the token this tab holds, once. A Cluster just founded may not answer for a few seconds, so
 * that refusal is asked again a while; any other ends the setup with its words, and the token stays for Try again.
 */
export async function runFirstSetup(organizationSlug: string, queryClient: QueryClient) {
  const held = readHeldCloudflare(organizationSlug);
  if (held === undefined || (held.kind !== "waiting" && held.kind !== "failed")) return;
  const { token, zone } = held;
  holdCloudflare(organizationSlug, { kind: "setting-up", token, zone });
  for (let tried = 1; ; tried++) {
    try {
      const row = await setUpCloudflareServerFn({ data: { organizationSlug, token: token.trim(), zone } });
      holdCloudflare(organizationSlug, { kind: "done", zone, switched: row.switching });
      toast.success(`Cloudflare Tunnel is set up on ${zone}.`);
      break;
    } catch (error) {
      const message = toErrorMessage(error, "Ployz Cloud didn't answer.");
      if (message.startsWith(SERVERS_UNANSWERED_START) && tried < ANSWER_TRIES) {
        await new Promise((resolve) => setTimeout(resolve, ANSWER_WAIT_MS));
        continue;
      }
      holdCloudflare(organizationSlug, { kind: "failed", token, zone, message });
      break;
    }
  }
  await queryClient.invalidateQueries({ queryKey: clusterIngressKeys.all });
}

/** Wherever the user goes in the Organization, the held setup runs as soon as the first Server founds the Cluster. */
export function useRunHeldCloudflareOnFounding(organizationSlug: string) {
  const queryClient = useQueryClient();
  const waiting = useHeldCloudflare(organizationSlug)?.kind === "waiting";
  const founded = useQuery({ ...clusterFoundedQueryOptions(organizationSlug), enabled: waiting }).data?.founded === true;
  useEffect(() => {
    if (waiting && founded) void runFirstSetup(organizationSlug, queryClient);
  }, [waiting, founded, organizationSlug, queryClient]);
}
