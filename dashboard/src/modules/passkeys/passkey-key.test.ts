import { generateKeyPairSync } from "node:crypto";
import { isoCBOR } from "@simplewebauthn/server/helpers";
import { describe, expect, it } from "vitest";
import { codeOfKeyBytes, deviceCodeOf } from "#/modules/devices/device-code";
import { codeOfPasskeyKey, passkeyKeyOf } from "#/modules/passkeys/passkey-key";

const jwkBytes = (value: string | undefined) => Buffer.from(value ?? "", "base64url");

function coseOf(entries: [number, number | Uint8Array][]) {
  return isoCBOR.encode(new Map(entries));
}

describe("passkeyKeyOf", () => {
  it("turns an ES256 COSE key into its SEC1 point and an EdDSA one into its raw bytes", () => {
    const ec = generateKeyPairSync("ec", { namedCurve: "P-256" }).publicKey.export({ format: "jwk" });
    const es256 = passkeyKeyOf(coseOf([[1, 2], [3, -7], [-1, 1], [-2, jwkBytes(ec.x)], [-3, jwkBytes(ec.y)]]));
    expect(es256?.algorithm).toBe("es256");
    expect(Buffer.from(es256?.key ?? "", "base64url")).toEqual(Buffer.concat([Buffer.of(4), jwkBytes(ec.x), jwkBytes(ec.y)]));

    const ed = generateKeyPairSync("ed25519").publicKey.export({ format: "jwk" });
    const eddsa = passkeyKeyOf(coseOf([[1, 1], [3, -8], [-1, 6], [-2, jwkBytes(ed.x)]]));
    expect(eddsa).toEqual({ algorithm: "eddsa", key: jwkBytes(ed.x).toString("base64url"), code: codeOfKeyBytes(jwkBytes(ed.x)) });
  });

  it("refuses keys a Server can't check", () => {
    const rsa = coseOf([[1, 3], [3, -257], [-1, new Uint8Array(256)], [-2, new Uint8Array(3)]]);
    expect(passkeyKeyOf(rsa)).toBeNull();
    expect(passkeyKeyOf(coseOf([[1, 2], [3, -7], [-1, 1], [-2, new Uint8Array(31)], [-3, new Uint8Array(32)]]))).toBeNull();
    expect(passkeyKeyOf(Uint8Array.of(0xff))).toBeNull();
  });

  it("codes a key by the same rule as a Device's key", () => {
    const hex = "07".repeat(32);
    expect(codeOfPasskeyKey({ algorithm: "eddsa", key: Buffer.from(hex, "hex").toString("base64url") })).toBe(deviceCodeOf(hex));
  });
});
