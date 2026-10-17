import { Schema } from "effect";
import { CHECKIN_WAIT_SECONDS, ProbeRequest } from "#/modules/machines/waiting/checkin-wire";

const REPORT_GRACE_MS = 3 * CHECKIN_WAIT_SECONDS * 1000;

export const TcpObservation = Schema.Union([
  Schema.Struct({ seen: Schema.Literals(["pending", "answered", "other_listener", "refused", "no_answer"]) }),
  Schema.Struct({ seen: Schema.Literal("not_probed"), reason: Schema.Literals(["private_range", "no_address", "address_unstable"]) }),
]);
export type TcpObservation = typeof TcpObservation.Type;

export const ProbeResults = Schema.Struct({
  request: ProbeRequest,
  handout: Schema.optionalKey(Schema.String),
  handedAt: Schema.optionalKey(Schema.String),
  address: Schema.optionalKey(Schema.String),
  moves: Schema.optionalKey(Schema.Int),
  tcp: Schema.Struct({ "80": TcpObservation, "443": TcpObservation }),
  host: Schema.optionalKey(Schema.Struct({ heardFrom: Schema.Array(Schema.String), bindFailed: Schema.Array(Schema.String) })),
});
export type ProbeResults = typeof ProbeResults.Type;

export type PortState =
  | { readonly state: "open" | "closed" }
  | {
    readonly state: "unknown";
    readonly reason:
      | "pending"
      | "private_range"
      | "no_address"
      | "address_unstable"
      | "no_answer"
      | "other_listener"
      | "port_busy"
      | "no_host_report";
  };

function portState(probe: ProbeResults, port: keyof PortStates, now: Date): PortState {
  const tcp = probe.tcp[port];
  if (tcp.seen === "not_probed") return { state: "unknown", reason: tcp.reason };
  if (probe.host?.bindFailed.includes(`tcp:${port}`)) return { state: "unknown", reason: "port_busy" };
  if (tcp.seen === "answered") return { state: "open" };
  const settled = probe.handedAt !== undefined
    && now.getTime() >= Date.parse(probe.handedAt) + probe.request.windowMs + REPORT_GRACE_MS;
  switch (tcp.seen) {
    case "pending":
      return { state: "unknown", reason: settled ? "no_answer" : "pending" };
    case "other_listener":
    case "no_answer":
      return { state: "unknown", reason: tcp.seen };
    case "refused":
      if (probe.host !== undefined) return { state: "closed" };
      return { state: "unknown", reason: settled ? "no_host_report" : "pending" };
  }
}

export interface PortStates {
  readonly "80": PortState;
  readonly "443": PortState;
}

export const portStates = (probe: ProbeResults, now: Date): PortStates => ({
  "80": portState(probe, "80", now),
  "443": portState(probe, "443", now),
});

/** The most waiting servers one probe or Reject takes at once. */
export const SELECTION_MAX = 1000;

export const OpenProbeSelectionInput = Schema.Struct({
  organizationSlug: Schema.String.check(Schema.isTrimmed(), Schema.isNonEmpty()),
  ids: Schema.Array(Schema.String.check(Schema.isUUID())).check(Schema.isMinLength(1), Schema.isMaxLength(SELECTION_MAX)),
});
export type OpenProbeSelectionInput = typeof OpenProbeSelectionInput.Type;
