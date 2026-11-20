import { Option, Schema } from "effect";
import { rustMachineIdSchema } from "#/modules/machines/enrollment";

/**
 * A Machine's bearer token for Cloud's OTLP routes: `ployz-otlp-v1.<payload>.<signature>`. The payload is the
 * base64url of `{"machine","iat","exp"}` JSON; the signature is the base64url of the Machine's management key signing
 * `ployz-otlp-v1\0` followed by the payload segment exactly as sent.
 */
const BEARER = /^Bearer ployz-otlp-v1\.([A-Za-z0-9_-]{1,512})\.([A-Za-z0-9_-]{86})$/;
const DOMAIN = Buffer.from("ployz-otlp-v1\0", "ascii");

export const TOKEN_MAX_SKEW_SECONDS = 300;
export const TOKEN_MAX_LIFETIME_SECONDS = 86_400;

const claims = Schema.Struct({
  machine: rustMachineIdSchema,
  iat: Schema.Int,
  exp: Schema.Int,
});

const decodeClaims = Schema.decodeUnknownOption(claims);

export type MachineToken = {
  readonly machine: Schema.Schema.Type<typeof claims>["machine"];
  readonly iat: number;
  readonly exp: number;
  /** The bytes the Machine signed. */
  readonly message: Buffer;
  readonly signature: Buffer;
};

/** The token an Authorization header carries, or none when it is malformed. Says nothing about whether it is signed or current. */
export function parseMachineToken(authorization: string | null): Option.Option<MachineToken> {
  const match = BEARER.exec(authorization ?? "");
  if (match === null) return Option.none();
  const [, payload, signature] = match;
  if (payload === undefined || signature === undefined) return Option.none();
  let json: unknown;
  try {
    json = JSON.parse(Buffer.from(payload, "base64url").toString("utf8"));
  } catch {
    return Option.none();
  }
  return Option.map(decodeClaims(json), (decoded) => ({
    ...decoded,
    message: Buffer.concat([DOMAIN, Buffer.from(payload, "ascii")]),
    signature: Buffer.from(signature, "base64url"),
  }));
}

/** Whether a token's claims hold at `nowSeconds`: not issued in the future, not expired, and short-lived. */
export function machineTokenCurrent({ iat, exp }: Pick<MachineToken, "iat" | "exp">, nowSeconds: number) {
  return iat <= nowSeconds + TOKEN_MAX_SKEW_SECONDS && exp > nowSeconds && exp > iat && exp - iat <= TOKEN_MAX_LIFETIME_SECONDS;
}
