import { Option, Schema } from "effect";

export const CRASH_EVENT_NAME = "ployz.container.exit";
export const MAX_CRASH_RECORDS = 1000;

// Unix nanos up to the last instant a Date can hold.
const MAX_MS = 8.64e15;
const Text = Schema.String.check(Schema.isMinLength(1), Schema.isMaxLength(256));

// OTLP/JSON writes an int64 as a decimal string, and proto3 JSON readers also take a number.
const Int64 = Schema.Union([Schema.String, Schema.Finite]);

const Value = Schema.Struct({
  stringValue: Schema.optionalKey(Schema.String),
  intValue: Schema.optionalKey(Int64),
  boolValue: Schema.optionalKey(Schema.Boolean),
});

const LogRecord = Schema.Struct({
  timeUnixNano: Schema.optionalKey(Int64),
  observedTimeUnixNano: Schema.optionalKey(Int64),
  attributes: Schema.optionalKey(Schema.Array(Schema.Struct({ key: Schema.String, value: Schema.optionalKey(Value) }))),
});

const Envelope = Schema.fromJsonString(Schema.Struct({
  resourceLogs: Schema.optionalKey(Schema.Array(Schema.Struct({
    scopeLogs: Schema.optionalKey(Schema.Array(Schema.Struct({ logRecords: Schema.optionalKey(Schema.Array(LogRecord)) }))),
  }))),
}));

const decodeEnvelope = Schema.decodeUnknownOption(Envelope);
const decodeText = Schema.decodeUnknownOption(Text);

/** One container exit a Machine reported, as Cloud keeps it. `eventId` is the Machine's `<container id>:<ts>`. */
export type CrashEventInput = {
  readonly eventId: string;
  readonly serviceId: string;
  readonly serviceName: string;
  readonly replica: string;
  readonly containerId: string;
  readonly deploymentId: string | null;
  readonly exitCode: number | null;
  readonly oom: boolean;
  readonly occurredAt: Date;
};

const nanosToMs = (nanos: typeof Int64.Type | undefined): Option.Option<number> => {
  const digits = String(nanos);
  if (!/^[1-9]\d{0,24}$/.test(digits)) return Option.none();
  const ms = BigInt(digits) / 1_000_000n;
  return ms <= BigInt(MAX_MS) ? Option.some(Number(ms)) : Option.none();
};

const exitCodeOf = (value: typeof Value.Type | undefined): Option.Option<number | null> => {
  if (value?.intValue === undefined) return Option.some(null);
  const digits = String(value.intValue);
  return /^-?\d{1,10}$/.test(digits) ? Option.some(Number(digits)) : Option.none();
};

const crashEvent = (record: typeof LogRecord.Type): Option.Option<CrashEventInput> => {
  const attributes = new Map(record.attributes?.map(({ key, value }) => [key, value]));
  if (attributes.get("event.name")?.stringValue !== CRASH_EVENT_NAME) return Option.none();
  const text = (key: string) => Option.flatMap(Option.fromNullishOr(attributes.get(key)?.stringValue), decodeText);
  const eventId = text("ployz.event.id");
  const namespace = text("ployz.namespace");
  const service = text("ployz.service.name");
  const serviceId = text("ployz.service.id");
  const containerId = text("container.id");
  const replica = attributes.get("ployz.replica")?.stringValue;
  const oom = attributes.get("ployz.exit.oom")?.boolValue;
  const deployment = attributes.get("ployz.deployment.id")?.stringValue;
  const exitCode = exitCodeOf(attributes.get("ployz.exit.code"));
  const at = Option.orElse(nanosToMs(record.timeUnixNano), () => nanosToMs(record.observedTimeUnixNano));
  if (
    Option.isNone(eventId) || Option.isNone(namespace) || Option.isNone(service) || Option.isNone(serviceId) || Option.isNone(containerId)
    || Option.isNone(exitCode) || Option.isNone(at) || replica === undefined || replica.length > 256 || oom === undefined
  ) return Option.none();
  return Option.some({
    eventId: eventId.value,
    serviceId: serviceId.value,
    serviceName: `${namespace.value}/${service.value}`,
    replica,
    containerId: containerId.value,
    deploymentId: deployment === undefined || deployment.length === 0 || deployment.length > 256 ? null : deployment,
    exitCode: exitCode.value,
    oom,
    occurredAt: new Date(at.value),
  });
};

/**
 * The container exits an OTLP/JSON logs body reports. A record that is not one, or lacks what an exit needs, is skipped
 * rather than failing the body, since the same body goes on to the Collector. Text that is not OTLP logs has none.
 */
export function crashEventsOf(json: string): ReadonlyArray<CrashEventInput> {
  const envelope = decodeEnvelope(json);
  if (Option.isNone(envelope)) return [];
  return (envelope.value.resourceLogs ?? [])
    .flatMap((resource) => resource.scopeLogs ?? [])
    .flatMap((scope) => scope.logRecords ?? [])
    .slice(0, MAX_CRASH_RECORDS)
    .flatMap((record) => Option.toArray(crashEvent(record)));
}
