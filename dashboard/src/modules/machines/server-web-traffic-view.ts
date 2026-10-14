import type { LatestWebTrafficChanges, WebTrafficWant } from "#/modules/machines/server-web-traffic";

/** What the Web traffic row shows: the switch's position, and the line under it. */
export type WebTrafficView =
  /** The Server takes web traffic: your domains point at its address. */
  | { readonly kind: "on"; readonly address: string | null }
  /** Turning on: the switch sits on, dimmed, while the web server starts. */
  | { readonly kind: "starting" }
  /** Turning off: the switch sits on, dimmed, while the web server stops. */
  | { readonly kind: "stopping" }
  | { readonly kind: "off" }
  /** The latest change didn't take: the switch sits where the Server is, with why above it and Retry. */
  | { readonly kind: "failed"; readonly want: WebTrafficWant; readonly reason: string; readonly address: string | null };

/** Whether a change is under way: the switch waits. */
export const webTrafficBusy = (view: WebTrafficView) => view.kind === "starting" || view.kind === "stopping";

/** Where the switch sits: on while it's on or on its way off, so it only moves when the Server does. */
export const webTrafficChecked = (view: WebTrafficView) =>
  view.kind === "on" || view.kind === "stopping" || (view.kind === "failed" && view.want === "off");

/** How long a finished change may take to show on the Server before the row trusts what the Server says instead. */
export const WEB_TRAFFIC_SETTLE_MS = 30_000;

/**
 * The row for `server`, from what the Server reports (`acceptsIngress`), its latest change, and the change this tab
 * asked for (`requested`, until its row arrives). A change under way shows as such; a finished one whose outcome the
 * Server hasn't shown yet keeps showing until `settled`; a failed one shows its reason only while the Server still
 * disagrees with what it wanted, so a failure the Server made good on its own leaves no alert.
 */
export function webTrafficView(input: {
  readonly server: { readonly id: string; readonly acceptsIngress: boolean; readonly publicIp: string | null };
  readonly latest: LatestWebTrafficChanges;
  readonly requested: { readonly id: string; readonly want: WebTrafficWant } | null;
  /** Whether the latest finished change has had its settle time. */
  readonly settled: boolean;
}): WebTrafficView {
  const { server, latest, requested, settled } = input;
  const observed: WebTrafficWant = server.acceptsIngress ? "on" : "off";
  const steady: WebTrafficView = observed === "on" ? { kind: "on", address: server.publicIp } : { kind: "off" };
  const own = latest[server.id] ?? null;
  if (requested !== null && own?.attemptId !== requested.id) return requested.want === "on" ? { kind: "starting" } : { kind: "stopping" };
  if (own === null) return steady;
  switch (own.state) {
    case "pending":
    case "running":
      return own.want === "on" ? { kind: "starting" } : { kind: "stopping" };
    case "finished":
      if (own.want === observed || settled) return steady;
      return own.want === "on" ? { kind: "starting" } : { kind: "stopping" };
    case "failed":
      if (own.want === observed) return steady;
      return { kind: "failed", want: own.want, reason: own.failure, address: server.publicIp };
  }
}
