/**
 * The shape of a Cloudflare tunnel token, as the CLI checks it before a Server runs a connector on it: base64 of JSON
 * naming the account (`a`), the tunnel (`t`, a UUID) and the connector's secret (`s`, base64). Anything else makes the
 * connector exit on start, so it is caught here, before the Cluster is asked to run it.
 */
import { Option, Schema } from "effect";

const BASE64 = /^[A-Za-z0-9+/]+={0,2}$/u;
const UUID = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/iu;

const Base64 = Schema.String.check(Schema.isPattern(BASE64));
const Base64OfSomething = Base64.check(Schema.makeFilter((value: string) => Buffer.from(value, "base64").length > 0));

const TunnelClaims = Schema.Struct({
  a: Schema.NonEmptyString,
  t: Schema.String.check(Schema.isPattern(UUID)),
  s: Base64OfSomething,
});

const decodeClaims = Schema.decodeUnknownOption(Schema.fromJsonString(TunnelClaims));

/** Whether `token` is a tunnel token a connector can start on. */
export function isTunnelToken(token: string): boolean {
  if (!BASE64.test(token)) return false;
  return Option.isSome(decodeClaims(Buffer.from(token, "base64").toString("utf8")));
}
