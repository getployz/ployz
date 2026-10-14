import type { IngressCheck } from "@ployz/sdk";
import { Schema } from "effect";
import { machineIdStringSchema } from "#/modules/machines/enrollment";

/** Which way the switch was flipped: on starts the web server on the Server, off stops it. */
export const WEB_TRAFFIC_WANTS = ["on", "off"] as const;
export type WebTrafficWant = (typeof WEB_TRAFFIC_WANTS)[number];

/**
 * One web traffic change Cloud applies to one Server, as its row moves: the flip writes `pending`, the run claims it
 * `running` right before the Engine is asked, and the Engine's answer ends it `finished` or `failed` with why.
 */
export const WEB_TRAFFIC_STATES = ["pending", "running", "finished", "failed"] as const;
export type WebTrafficState = (typeof WEB_TRAFFIC_STATES)[number];

/** A request no run picked up in this long never starts; the hourly sweep closes it. */
export const WEB_TRAFFIC_PENDING_LIMIT_MS = 15 * 60_000;
/** The Engine's answer takes at most this long: a role wait and a proxy start. Past it, the change ends failed. */
export const WEB_TRAFFIC_RUNNING_LIMIT_MS = 3 * 60_000;

export const RequestServerWebTrafficInput = Schema.Struct({
  organizationSlug: Schema.String.check(Schema.isNonEmpty()),
  machineId: machineIdStringSchema,
  /** Minted by the flipping tab: the row's id, so sending the same request again returns the same change. */
  requestId: Schema.String.check(Schema.isUUID()),
  want: Schema.Literals(WEB_TRAFFIC_WANTS),
});
export type RequestServerWebTrafficInput = typeof RequestServerWebTrafficInput.Type;

/** One Server's latest web traffic change as the page reads it: each state with exactly its evidence. */
export type LatestWebTraffic =
  | { readonly attemptId: string; readonly want: WebTrafficWant; readonly state: "pending"; readonly requestedAt: string }
  | { readonly attemptId: string; readonly want: WebTrafficWant; readonly state: "running"; readonly startedAt: string }
  | { readonly attemptId: string; readonly want: WebTrafficWant; readonly state: "finished"; readonly endedAt: string }
  | { readonly attemptId: string; readonly want: WebTrafficWant; readonly state: "failed"; readonly endedAt: string; readonly failure: string };

/** The change a request admitted. */
export type RequestedServerWebTraffic = { readonly change: LatestWebTraffic };

/** Every Server's latest web traffic change, by Machine ID; a Server never switched from Cloud has none. */
export type LatestWebTrafficChanges = Readonly<Record<string, LatestWebTraffic>>;

/**
 * The port Docker couldn't bind, as its error names it: `0.0.0.0:80: bind: address already in use` when a process
 * holds it, `Bind for 0.0.0.0:80 failed: port is already allocated` when another container does.
 */
const takenPort = (cause: string) => /:(\d+)(?::\s*bind|\s+failed:\s*port is already allocated)/.exec(cause)?.[1] ?? null;

/**
 * Why the web server didn't start on this Server, from what the start did on every Server holding the role. Null when
 * it started here, or already ran.
 */
export function startFailure(check: IngressCheck, machineId: string): string | null {
  const repair = check.repairs.find((entry) => entry.machine === machineId);
  if (repair === undefined) {
    return check.unchecked.length > 0 ? "Couldn't check whether the web server started. Try again." : null;
  }
  if (repair.failure === null) return null;
  if (repair.failure.type === "port_taken") {
    const port = takenPort(repair.failure.cause);
    return `${port === null ? "The web port is" : `Port ${port} is`} in use on ${repair.server}. Free it, then try again.`;
  }
  return `The web server didn't start on ${repair.server}: ${repair.failure.cause}`;
}
