import { isoCBOR } from "@simplewebauthn/server/helpers";
import { codeOfKeyBytes } from "#/modules/devices/device-code";

/**
 * A passkey's public key as a Server stores it: an ES256 key as its SEC1 uncompressed point, an EdDSA key as its 32 raw
 * bytes, both in unpadded base64url. Servers never read COSE, so Cloud converts once, here.
 */
export type PasskeyKey = { readonly algorithm: "es256" | "eddsa"; readonly key: string };

const COSE = { kty: 1, alg: 3, crv: -1, x: -2, y: -3 } as const;

/** The key and its code from a COSE_Key, or null for any key but P-256 ES256 or Ed25519 EdDSA. */
export function passkeyKeyOf(cose: Uint8Array): (PasskeyKey & { readonly code: string }) | null {
  let map: Map<number, unknown>;
  try {
    map = isoCBOR.decodeFirst<Map<number, unknown>>(new Uint8Array(cose));
  } catch {
    return null;
  }
  const bytes = (label: number) => {
    const value = map.get(label);
    return value instanceof Uint8Array && value.length === 32 ? value : null;
  };
  const x = bytes(COSE.x);
  if (x === null) return null;
  if (map.get(COSE.kty) === 2 && map.get(COSE.alg) === -7 && map.get(COSE.crv) === 1) {
    const y = bytes(COSE.y);
    if (y === null) return null;
    return withCode({ algorithm: "es256", key: Buffer.concat([Buffer.of(4), x, y]).toString("base64url") });
  }
  if (map.get(COSE.kty) === 1 && map.get(COSE.alg) === -8 && map.get(COSE.crv) === 6) {
    return withCode({ algorithm: "eddsa", key: Buffer.from(x).toString("base64url") });
  }
  return null;
}

/** The code a person reads out for a key in a Server's form, as the Engine's `PasskeyPublicKey::code` computes it. */
export function codeOfPasskeyKey(key: PasskeyKey) {
  return codeOfKeyBytes(Buffer.from(key.key, "base64url"));
}

const withCode = (key: PasskeyKey) => ({ ...key, code: codeOfPasskeyKey(key) });
