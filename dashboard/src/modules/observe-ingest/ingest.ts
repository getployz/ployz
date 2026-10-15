import { Option, Schema } from "effect";
import { OBSERVE_STREAMS } from "#/modules/observe-ingest/tables";

export const MAX_PUSH_EVENTS = 1000;

const MAX_SEQ = 2n ** 63n - 1n;
// Unix nanos up to the last instant a Date can hold.
const MAX_TS = 8.64e21;

// A u64 sent as a decimal string, because unix nanos exceed 2^53. Postgres bigint holds up to 2^63 - 1.
const Seq = Schema.String.check(Schema.isPattern(/^(0|[1-9]\d{0,18})$/));

const Text = Schema.String.check(Schema.isMinLength(1), Schema.isMaxLength(256));

const Marker = Schema.Struct({
  container_id: Text,
  service_id: Text,
  service: Text,
  deployment_id: Schema.optionalKey(Text),
  replica: Schema.String.check(Schema.isMaxLength(256)),
  ts: Schema.Finite.check(Schema.isBetween({ minimum: 0, maximum: MAX_TS })),
  exit_code: Schema.optionalKey(Schema.NullOr(Schema.Finite.check(Schema.isInt()))),
  oom: Schema.optionalKey(Schema.Boolean),
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
  readonly dropped: number;
  readonly events: ReadonlyArray<CrashEventInput>;
};

const toSeq = (value: string): Option.Option<bigint> => {
  const seq = BigInt(value);
  return seq <= MAX_SEQ ? Option.some(seq) : Option.none();
};

export const crashPushInput = (push: CrashPush): Option.Option<CrashPushInput> => {
  const seq = toSeq(push.seq);
  if (Option.isNone(seq)) return Option.none();
  const events: Array<CrashEventInput> = [];
  for (const { seq: eventSeq, marker } of push.events) {
    const seqValue = toSeq(eventSeq);
    if (Option.isNone(seqValue)) return Option.none();
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
  return Option.some({ seq: seq.value, dropped: push.dropped, events });
};

const Day = Schema.String.check(Schema.isPattern(/^\d{4}-\d{2}-\d{2}$/));
const Count = Schema.Finite.check(Schema.isInt(), Schema.isGreaterThanOrEqualTo(0));
const QueryValue = Schema.optionalKey(Schema.String.check(Schema.isMaxLength(1024)));

const PageviewRow = Schema.Struct({
  kind: Schema.Literal("pageview"),
  ts: Schema.Finite.check(Schema.isInt(), Schema.isBetween({ minimum: 0, maximum: MAX_TS / 1e6 })),
  namespace: Text,
  host: Schema.String.check(Schema.isMaxLength(1024)),
  path: Schema.String.check(Schema.isMaxLength(4096)),
  query: Schema.Struct({
    utm_source: QueryValue,
    utm_medium: QueryValue,
    utm_campaign: QueryValue,
    utm_term: QueryValue,
    utm_content: QueryValue,
    ref: QueryValue,
  }),
  referrer_host: Schema.NullOr(Schema.String.check(Schema.isMaxLength(1024))),
  language: Schema.NullOr(Schema.String.check(Schema.isMaxLength(256))),
  visitor: Schema.String.check(Schema.isPattern(/^[0-9a-f]{64}$/)),
  flags: Schema.Array(Schema.Literals(["no_accept_language", "no_sec_fetch", "click_id"])),
});

const BotsRow = Schema.Struct({ kind: Schema.Literal("bots"), namespace: Text, day: Day, reason: Text, count: Count });
const GapRow = Schema.Struct({ kind: Schema.Literal("gap"), namespace: Text, day: Schema.NullOr(Day), count: Count });

const PageviewPush = Schema.Struct({
  stream: Schema.Literal("pageviews"),
  seq: Seq,
  dropped: Schema.Finite,
  events: Schema.Array(Schema.Union([PageviewRow, BotsRow, GapRow])).check(Schema.isMaxLength(MAX_PUSH_EVENTS)),
});
type PageviewPush = typeof PageviewPush.Type;

export const decodePageviewPush = Schema.decodeUnknownOption(PageviewPush);

const FLAG_BITS = { no_accept_language: 1, no_sec_fetch: 2, click_id: 4 } as const;

export function visitorId(visitor: string) {
  return BigInt.asIntN(64, BigInt(`0x${visitor.slice(0, 16)}`));
}

export type PageviewInput = {
  readonly kind: "pageview";
  readonly namespace: string;
  readonly ts: Date;
  readonly visitorId: bigint;
  readonly host: string;
  readonly pathname: string;
  readonly source: string;
  readonly referrerHost: string | null;
  readonly utmSource: string | null;
  readonly utmMedium: string | null;
  readonly utmCampaign: string | null;
  readonly utmTerm: string | null;
  readonly utmContent: string | null;
  readonly ref: string | null;
  readonly language: string | null;
  readonly flags: number;
};
export type BotsInput = { readonly kind: "bots"; readonly namespace: string; readonly day: string; readonly reason: string; readonly count: number };
export type GapInput = { readonly kind: "gap"; readonly namespace: string; readonly day: string | null; readonly count: number };

export type PageviewPushInput = {
  readonly seq: bigint;
  readonly dropped: number;
  readonly events: ReadonlyArray<PageviewInput | BotsInput | GapInput>;
};

export const pageviewPushInput = (push: PageviewPush): Option.Option<PageviewPushInput> =>
  Option.map(toSeq(push.seq), (seq) => ({
    seq,
    dropped: push.dropped,
    events: push.events.map((row): PageviewInput | BotsInput | GapInput => {
      if (row.kind !== "pageview") return row;
      const { query } = row;
      return {
        kind: "pageview",
        namespace: row.namespace,
        ts: new Date(row.ts),
        visitorId: visitorId(row.visitor),
        host: row.host,
        pathname: row.path,
        source: query.utm_source ?? query.ref ?? row.referrer_host ?? "",
        referrerHost: row.referrer_host,
        utmSource: query.utm_source ?? null,
        utmMedium: query.utm_medium ?? null,
        utmCampaign: query.utm_campaign ?? null,
        utmTerm: query.utm_term ?? null,
        utmContent: query.utm_content ?? null,
        ref: query.ref ?? null,
        language: row.language,
        flags: row.flags.reduce((bits, flag) => bits | FLAG_BITS[flag], 0),
      };
    }),
  }));
