import type { LatestWebTrafficChanges, WebTrafficWant } from "#/modules/machines/server-web-traffic";
import { needsAttention, type ServerStatus } from "#/modules/machines/server-status";

/** What the Web traffic row shows: the switch's position, and the line under it. */
export type WebTrafficView =
  /** The Server takes web traffic: your domains point at its address. */
  | { readonly kind: "on"; readonly address: string | null }
  /** Turning on: the switch sits on, dimmed, while the web server starts. */
  | { readonly kind: "starting" }
  /** Turning off: the switch sits on, dimmed, while the web server stops. */
  | { readonly kind: "stopping" }
  | { readonly kind: "off" }
  /** The latest change didn't take: the switch sits where the Server is (`on`), with why above it and Retry. */
  | { readonly kind: "failed"; readonly want: WebTrafficWant; readonly on: boolean; readonly reason: string; readonly address: string | null };

/** Whether a change is under way: the switch waits. */
export const webTrafficBusy = (view: WebTrafficView) => view.kind === "starting" || view.kind === "stopping";

/** Where the switch sits: on while it's on or on its way off, so it only moves when the Server does. */
export const webTrafficChecked = (view: WebTrafficView) =>
  view.kind === "on" || view.kind === "stopping" || (view.kind === "failed" && view.on);

type IngressServer = { readonly id: string; readonly acceptsIngress: boolean; readonly publicIp: string | null };

/** Turning `server` off stops every domain: it takes web traffic now, and no other online Server would. */
export const lastWebTrafficServer = (
  server: IngressServer,
  servers: ReadonlyArray<{ readonly machine: IngressServer; readonly status: ServerStatus }>,
) =>
  server.acceptsIngress &&
  server.publicIp !== null &&
  !servers.some((other) =>
    other.machine.id !== server.id && other.machine.acceptsIngress && other.machine.publicIp !== null && !needsAttention(other.status));

/** How long a finished change may take to show on the Server before the row trusts what the Server says instead. */
export const WEB_TRAFFIC_SETTLE_MS = 30_000;

/**
 * The row for `server`, from what the Server reports (`acceptsIngress`), its latest change, and the change this tab
 * asked for (`requested`, until its row arrives). A change under way shows as such; a finished one whose outcome the
 * Server hasn't shown yet keeps showing until `settled`. A failed change always shows its reason. After a failed stop
 * the role may be off while the web server still runs. After a failed start the role is on only when putting it back
 * failed too, and then your domains point at a Server whose web server may not run.
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
      return { kind: "failed", want: own.want, on: observed === "on", reason: own.failure, address: server.publicIp };
  }
}
