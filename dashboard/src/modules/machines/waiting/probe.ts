import { Schema } from "effect";
import { ProbeRequest } from "#/modules/machines/waiting/checkin-wire";

export const PortProbe = Schema.Union([
  Schema.Struct({ state: Schema.Literals(["open", "closed"]) }),
  Schema.Struct({ state: Schema.Literal("unknown"), reason: Schema.Literals(["pending", "private_range", "no_address", "no_answer"]) }),
]);
export type PortProbe = typeof PortProbe.Type;

export const ProbeResults = Schema.Struct({
  request: ProbeRequest,
  handedAt: Schema.optionalKey(Schema.String),
  tcp: Schema.Struct({ "80": PortProbe, "443": PortProbe }),
  host: Schema.optionalKey(Schema.Struct({ heardFrom: Schema.Array(Schema.String), bindFailed: Schema.Array(Schema.String) })),
});
export type ProbeResults = typeof ProbeResults.Type;

export const OpenProbeSelectionInput = Schema.Struct({
  organizationSlug: Schema.String.check(Schema.isTrimmed(), Schema.isNonEmpty()),
  ids: Schema.Array(Schema.String.check(Schema.isUUID())).check(Schema.isMinLength(1), Schema.isMaxLength(1000)),
});
export type OpenProbeSelectionInput = typeof OpenProbeSelectionInput.Type;
