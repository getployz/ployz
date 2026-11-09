import { useSyncExternalStore } from "react";

/**
 * A Cloudflare choice made before the Cluster exists. The token lives in this tab's memory only, since Cloud keeps no
 * Cloudflare credential: setup needs the Cluster's Sealing Key, so it runs with the token once the first Server joins,
 * and the token is dropped when it ends.
 */
export type HeldCloudflare =
  | { readonly kind: "waiting" | "setting-up"; readonly token: string; readonly zone: string }
  | { readonly kind: "failed"; readonly token: string; readonly zone: string; readonly message: string }
  | { readonly kind: "done"; readonly zone: string };

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
