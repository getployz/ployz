import type { CloudflareSetup, ClusterIngressObservation } from "#/modules/ingress/doors";
import type { ServerListState } from "#/modules/machines/server-status";
import type { HeldCloudflare } from "#/modules/ingress/cloudflare-setup";

/** What the Servers page shows. */
export type ServersPageState =
  | { readonly kind: "loading" }
  | { readonly kind: "unreachable" }
  | { readonly kind: "first-run" }
  | { readonly kind: "fleet"; readonly stale: boolean };

export function serversPageState({ list, servers }: { list: ServerListState; servers: number }): ServersPageState {
  if (list === "loading" || list === "unreachable") return { kind: list };
  if (servers === 0) return { kind: "first-run" };
  return { kind: "fleet", stale: list === "stale" };
}

/**
 * Where a Cloudflare Tunnel chosen on the first-run card stands. `waiting` until the first Server joins, then the setup
 * this tab runs with the token it holds. `unfinished` once a first Server founded the Cluster closed and no Server holds a
 * Door, with no token left to set the tunnel up, as after a reload: nothing reaches its apps until the user acts.
 */
export type FirstTunnelState =
  | { readonly kind: "none" }
  | { readonly kind: "waiting" | "setting-up"; readonly zone: string }
  | { readonly kind: "done"; readonly zone: string; readonly switched: boolean }
  | { readonly kind: "failed"; readonly zone: string; readonly message: string }
  | { readonly kind: "unfinished" };

export function firstTunnelState({ held, foundedClosed, setup, observed }: {
  held: HeldCloudflare | undefined;
  /** Cloud told the first Server to found with every Door closed. */
  foundedClosed: boolean;
  setup: CloudflareSetup | null;
  /** The Servers seen and the Doors each holds; null before a look, or when none answered. */
  observed: ClusterIngressObservation | null;
}): FirstTunnelState {
  if (held !== undefined) {
    if (held.kind === "failed") return { kind: "failed", zone: held.zone, message: held.message };
    return held.kind === "done" ? held : { kind: held.kind, zone: held.zone };
  }
  if (!foundedClosed || setup !== null || observed === null || observed.partial || observed.servers.length === 0) return { kind: "none" };
  return observed.servers.every((server) => server.doors.length === 0) ? { kind: "unfinished" } : { kind: "none" };
}
