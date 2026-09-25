import "@tanstack/react-start/server-only";
import { generateKeyPairSync, sign } from "node:crypto";

// ponytail: Node can sign but not build a PKCS#10 CSR; these few DER bytes replace a dependency.
const der = (tag: number, ...parts: Buffer[]) => {
  const body = Buffer.concat(parts);
  const n = body.length; // a CSR stays far below 64 KiB
  const length = n < 0x80 ? [n] : n < 0x100 ? [0x81, n] : [0x82, n >> 8, n & 0xff];
  return Buffer.concat([Buffer.from([tag, ...length]), body]);
};
const sequence = (...parts: Buffer[]) => der(0x30, ...parts);
const oid = (hex: string) => Buffer.from(`06${(hex.length / 2).toString(16).padStart(2, "0")}${hex}`, "hex");

const EXTENSION_REQUEST = oid("2a864886f70d01090e"); // 1.2.840.113549.1.9.14
const SUBJECT_ALT_NAME = oid("551d11"); // 2.5.29.17
const ECDSA_WITH_SHA256 = sequence(oid("2a8648ce3d040302")); // 1.2.840.10045.4.3.2

const pem = (label: string, body: Buffer) =>
  `-----BEGIN ${label}-----\n${body.toString("base64").match(/.{1,64}/gu)?.join("\n")}\n-----END ${label}-----\n`;

/**
 * A fresh P-256 key and a CSR for exactly `name` and `*.name`, with an empty subject:
 * Hosted DNS refuses a CSR naming anything else, including a CN outside those two names.
 */
export function createWildcardCsr(name: string) {
  const { privateKey, publicKey } = generateKeyPairSync("ec", { namedCurve: "P-256" });
  const dnsNames = [name, `*.${name}`].map((host) => der(0x82, Buffer.from(host, "ascii")));
  const info = sequence(
    der(0x02, Buffer.from([0])),
    sequence(),
    publicKey.export({ type: "spki", format: "der" }),
    der(0xa0, sequence(EXTENSION_REQUEST, der(0x31, sequence(sequence(SUBJECT_ALT_NAME, der(0x04, sequence(...dnsNames))))))),
  );
  const signature = sign("sha256", info, privateKey);
  return {
    privateKeyPem: privateKey.export({ type: "pkcs8", format: "pem" }).toString(),
    csrPem: pem("CERTIFICATE REQUEST", sequence(info, ECDSA_WITH_SHA256, der(0x03, Buffer.from([0]), signature))),
  };
}
