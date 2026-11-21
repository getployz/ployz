import { Option, Schema } from "effect";
import { OBSERVE_STREAMS } from "#/modules/observe-ingest/tables";

export const MAX_PUSH_EVENTS = 1000;

const MAX_SEQ = 2n ** 63n - 1n;
// Unix nanos up to the last instant a Date can hold.
const MAX_TS = 8.64e21;

// A u64 sent as a decimal string, because unix nanos exceed 2^53. Postgres bigint holds up to 2^63 - 1.
const Seq = Schema.String.check(Schema.isPattern(/^(0|[1-9]\d{0,18})$/));

const Text = Schema.String.check(Schema.isMinLength(1), Schema.isMaxLength(256));

export const decodePushHeader = Schema.decodeUnknownOption(Schema.Struct({ stream: Schema.String }));
export const isObserveStream = Schema.is(Schema.Literals(OBSERVE_STREAMS));

const toSeq = (value: string): Option.Option<bigint> => {
  const seq = BigInt(value);
  return seq <= MAX_SEQ ? Option.some(seq) : Option.none();
};

const QueryValue = Schema.optionalKey(Schema.String.check(Schema.isMaxLength(1024)));
const Count = Schema.Finite.check(Schema.isInt(), Schema.isGreaterThanOrEqualTo(0));
const Header = Schema.NullOr(Schema.String.check(Schema.isMaxLength(512)));

const RequestRow = Schema.Struct({
  ts: Schema.Finite.check(Schema.isInt(), Schema.isBetween({ minimum: 0, maximum: MAX_TS / 1e6 })),
  namespace: Text,
  service: Text,
  host: Schema.String.check(Schema.isMaxLength(1024)),
  method: Schema.String.check(Schema.isMinLength(1), Schema.isMaxLength(32)),
  path: Schema.String.check(Schema.isMaxLength(4096)),
  query: Schema.Struct({
    utm_source: QueryValue,
    utm_medium: QueryValue,
    utm_campaign: QueryValue,
    utm_term: QueryValue,
    utm_content: QueryValue,
    ref: QueryValue,
  }),
  status: Schema.Finite.check(Schema.isInt(), Schema.isBetween({ minimum: 0, maximum: 999 })),
  duration_us: Count,
  bytes_in: Count,
  bytes_out: Count,
  request_id: Schema.String.check(Schema.isMaxLength(256)),
  ua: Schema.String.check(Schema.isMaxLength(1024)),
  referrer_host: Schema.NullOr(Schema.String.check(Schema.isMaxLength(1024))),
  language: Header,
  content_type: Header,
  sec_purpose: Header,
  visitor: Schema.NullOr(Schema.String.check(Schema.isPattern(/^[0-9a-f]{64}$/))),
});

export type RequestWireRow = typeof RequestRow.Encoded;

const RequestPush = Schema.Struct({
  stream: Schema.Literal("requests"),
  seq: Seq,
  dropped: Schema.Finite,
  rows: Schema.Array(RequestRow).check(Schema.isMaxLength(MAX_PUSH_EVENTS)),
});
type RequestPush = typeof RequestPush.Type;

export const decodeRequestPush = Schema.decodeUnknownOption(RequestPush);

export function visitorId(visitor: string) {
  return BigInt.asIntN(64, BigInt(`0x${visitor.slice(0, 16)}`));
}

export type RequestInput = {
  readonly namespace: string;
  readonly ts: Date;
  readonly requestId: string;
  readonly service: string;
  readonly host: string;
  readonly method: string;
  readonly path: string;
  readonly status: number;
  readonly durationMs: number;
  readonly bytesIn: number;
  readonly bytesOut: number;
  readonly visitorId: bigint | null;
  readonly source: string;
  readonly referrerHost: string | null;
  readonly language: string | null;
  readonly utmSource: string | null;
  readonly utmMedium: string | null;
  readonly utmCampaign: string | null;
  readonly utmTerm: string | null;
  readonly utmContent: string | null;
  readonly ua: string;
  readonly contentType: string | null;
  readonly secPurpose: string | null;
};

export type RequestPushInput = {
  readonly seq: bigint;
  readonly dropped: number;
  readonly rows: ReadonlyArray<RequestInput>;
};

export const requestPushInput = (push: RequestPush): Option.Option<RequestPushInput> =>
  Option.map(toSeq(push.seq), (seq) => ({
    seq,
    dropped: push.dropped,
    rows: push.rows.map(({ query, ...row }, index): RequestInput => ({
      namespace: row.namespace,
      ts: new Date(row.ts),
      // A replayed batch keeps its seq, so a row without an id lands on the same key again.
      requestId: row.request_id || `${push.seq}:${index}`,
      service: row.service,
      host: row.host,
      method: row.method,
      path: row.path,
      status: row.status,
      durationMs: row.duration_us / 1000,
      bytesIn: row.bytes_in,
      bytesOut: row.bytes_out,
      visitorId: row.visitor === null ? null : visitorId(row.visitor),
      source: query.utm_source ?? query.ref ?? row.referrer_host ?? "",
      referrerHost: row.referrer_host,
      language: row.language,
      utmSource: query.utm_source ?? null,
      utmMedium: query.utm_medium ?? null,
      utmCampaign: query.utm_campaign ?? null,
      utmTerm: query.utm_term ?? null,
      utmContent: query.utm_content ?? null,
      ua: row.ua,
      contentType: row.content_type,
      secPurpose: row.sec_purpose,
    })),
  }));
