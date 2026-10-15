import { Option, Schema } from "effect";
import { OBSERVE_STREAMS } from "#/modules/observe-ingest/tables";

export const MAX_PUSH_EVENTS = 1000;

const MAX_SEQ = 2n ** 63n - 1n;

// A u64 sent as a decimal string, because unix nanos exceed 2^53. Postgres bigint holds up to 2^63 - 1.
const Seq = Schema.String.check(Schema.isPattern(/^(0|[1-9]\d{0,18})$/));

const Text = Schema.String.check(Schema.isMinLength(1), Schema.isMaxLength(256));

const Marker = Schema.Struct({
  container_id: Text,
  service_id: Text,
  service: Text,
  deployment_id: Schema.optionalKey(Text),
  replica: Schema.String.check(Schema.isMaxLength(256)),
  ts: Schema.Finite,
  kind: Text,
  exit_code: Schema.optionalKey(Schema.NullOr(Schema.Finite.check(Schema.isInt()))),
  oom: Schema.optionalKey(Schema.Boolean),
  stopped: Schema.optionalKey(Schema.Boolean),
});

const CrashPush = Schema.Struct({
  stream: Schema.Literal("crash"),
  seq: Seq,
  dropped: Schema.Finite,
  events: Schema.Array(Schema.Struct({ seq: Seq, marker: Marker })).check(Schema.isMaxLength(MAX_PUSH_EVENTS)),
});
type CrashPush = typeof CrashPush.Type;

export const decodePushHeader = Schema.decodeUnknownOption(Schema.Struct({ stream: Schema.String }));
export const isObserveStream = Schema.is(Schema.Literals(OBSERVE_STREAMS));
export const decodeCrashPush = Schema.decodeUnknownOption(CrashPush);

export type CrashEventInput = {
  readonly seq: bigint;
  readonly serviceId: string;
  readonly serviceName: string;
  readonly replica: string;
  readonly containerId: string;
  readonly deploymentId: string | null;
  readonly exitCode: number | null;
  readonly oom: boolean;
  readonly occurredAt: Date;
};

export type CrashPushInput = {
  readonly seq: bigint;
  readonly events: ReadonlyArray<CrashEventInput>;
};

const toSeq = (value: string): Option.Option<bigint> => {
  const seq = BigInt(value);
  return seq <= MAX_SEQ ? Option.some(seq) : Option.none();
};

type Marker = typeof Marker.Type;

const isCrash = (marker: Marker) =>
  marker.kind === "exit"
  && (marker.oom === true || (marker.exit_code != null && marker.exit_code !== 0 && marker.stopped !== true));

export const crashPushInput = (push: CrashPush): Option.Option<CrashPushInput> => {
  const seq = toSeq(push.seq);
  if (Option.isNone(seq)) return Option.none();
  const events: Array<CrashEventInput> = [];
  for (const { seq: eventSeq, marker } of push.events) {
    const seqValue = toSeq(eventSeq);
    if (Option.isNone(seqValue)) return Option.none();
    if (!isCrash(marker)) continue;
    events.push({
      seq: seqValue.value,
      serviceId: marker.service_id,
      serviceName: marker.service,
      replica: marker.replica,
      containerId: marker.container_id,
      deploymentId: marker.deployment_id ?? null,
      exitCode: marker.exit_code ?? null,
      oom: marker.oom === true,
      occurredAt: new Date(Math.floor(marker.ts / 1e6)),
    });
  }
  return Option.some({ seq: seq.value, events });
};
