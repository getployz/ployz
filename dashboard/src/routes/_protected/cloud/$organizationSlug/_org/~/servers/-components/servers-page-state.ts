import type { ServerListState } from "#/modules/machines/server-status";

/**
 * What the Servers page shows. The intake tray (servers waiting or enrolling) comes from Cloud, not the Runtime, so it
 * shows whenever something is in it, even while the Runtime is loading or unreachable.
 */
export type ServersPageState =
  | { readonly kind: "loading"; readonly intake: boolean }
  | { readonly kind: "unreachable"; readonly intake: boolean }
  | { readonly kind: "first-run"; readonly intake: boolean }
  | { readonly kind: "fleet"; readonly intake: boolean; readonly stale: boolean };

export function serversPageState({ list, servers, waiting, enrolling }: {
  list: ServerListState;
  servers: number;
  /** Undefined until Cloud answers. */
  waiting: number | undefined;
  enrolling: number | undefined;
}): ServersPageState {
  const intake = (waiting ?? 0) + (enrolling ?? 0) > 0;
  if (list === "loading" || list === "unreachable") return { kind: list, intake };
  // Without Cloud's answer, zero servers could still be a first arrival, so the first-run card would mislead.
  if (servers === 0) return waiting === undefined || enrolling === undefined ? { kind: "loading", intake } : { kind: "first-run", intake };
  return { kind: "fleet", intake, stale: list === "stale" };
}
