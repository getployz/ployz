import { SealError } from "#/modules/sealing/seal-error";

/** HPKE `info` and the first associated-data field of a value sealed to a Cluster's Sealing Key. */
const SEALED_VALUE_INFO = "ployz-sealed-v1";
/** The Store's `MAX_ENV_STRING`: Linux passes at most 128 KiB per variable, `KEY=value` and its NUL included. */
const MAX_ENV_STRING = 128 * 1024;
/** ployz-core's `scrub::SHORT_VALUE`: a log row shorter than this can't be scrubbed on its own. */
const SHORT_ROW = 6;

const encoder = new TextEncoder();

/**
 * Why `value` can't be sealed in the browser for `key`, or null. The Store and the Server refuse these too; this
 * says so before the value leaves the page.
 */
export function sealedValueProblem(key: string, value: string): string | null {
  if (key === "PORT") return "PORT can't be sealed: Ployz reads it to route traffic.";
  // The Store refuses a NUL in a plain value, and can't see one inside a sealed value: no environment can hold it.
  if (value.includes("\0")) return "A value can't hold a null character.";
  if (value.includes("${{")) return "A sealed value is stored as-is. Untick Sealed to use a reference.";
  if (hasShortRow(value)) return "Each line of a multiline sealed value needs at least 6 characters.";
  if (encoder.encode(key).length + encoder.encode(value).length + 2 > MAX_ENV_STRING) {
    return "Too long: a variable holds at most 128 KiB, name included.";
  }
  return null;
}

/** ployz-core's `scrub::has_short_row`: rows split as Docker's log copier does, so a trailing LF starts none. */
function hasShortRow(value: string) {
  const rows = value.split("\n");
  if (rows.at(-1) === "") rows.pop();
  return rows.length > 1 && rows.some((row) => {
    const bytes = encoder.encode(row).length;
    return bytes >= 1 && bytes < SHORT_ROW;
  });
}

let suite: Promise<import("@hpke/core").CipherSuite> | undefined;

/** Loads the HPKE suite ahead of the first seal. Holds no key. */
export function warmSealing() {
  suite ??= Promise.all([import("@hpke/core"), import("@hpke/dhkem-x25519"), import("@hpke/chacha20poly1305")]).then(
    ([{ CipherSuite }, { DhkemX25519HkdfSha256, HkdfSha256 }, { Chacha20Poly1305 }]) =>
      new CipherSuite({ kem: new DhkemX25519HkdfSha256(), kdf: new HkdfSha256(), aead: new Chacha20Poly1305() }),
  );
  suite.catch(() => {
    suite = undefined;
  });
  return suite;
}

/**
 * `plaintext` sealed for variable `key` of `target.service` in `target.environment`, to the Cluster key
 * `recipient.publicKey`: base64 of the encapsulated key followed by the ciphertext, as ployzd's `open_value` reads it.
 */
export async function sealValue(
  recipient: { readonly organizationId: string; readonly publicKey: string },
  target: { readonly environment: string; readonly service: string },
  key: string,
  plaintext: string,
): Promise<string> {
  const problem = sealedValueProblem(key, plaintext);
  if (problem !== null) throw new SealError(problem);
  const hpke = await warmSealing();
  const recipientPublicKey = await hpke.kem.deserializePublicKey(fromBase64(recipient.publicKey));
  const { enc, ct } = await hpke.seal(
    { recipientPublicKey, info: encoder.encode(SEALED_VALUE_INFO) },
    encoder.encode(plaintext),
    associatedData(recipient.organizationId, target, key),
  );
  const out = new Uint8Array(enc.byteLength + ct.byteLength);
  out.set(new Uint8Array(enc), 0);
  out.set(new Uint8Array(ct), enc.byteLength);
  return toBase64(out);
}

/** ployz-core's `SealedValue::associated_data` for a value scoped to one Service. */
function associatedData(organizationId: string, target: { environment: string; service: string }, key: string) {
  const fields = [
    encoder.encode(SEALED_VALUE_INFO),
    encoder.encode(organizationId),
    uuidBytes(target.environment),
    encoder.encode("service"),
    uuidBytes(target.service),
    encoder.encode(key),
  ];
  const out = new Uint8Array(fields.reduce((length, field) => length + 4 + field.length, 0));
  const view = new DataView(out.buffer);
  let offset = 0;
  for (const field of fields) {
    view.setUint32(offset, field.length);
    out.set(field, offset + 4);
    offset += 4 + field.length;
  }
  return out;
}

function uuidBytes(uuid: string) {
  const hex = uuid.replaceAll("-", "");
  if (!/^[0-9a-f]{32}$/i.test(hex)) throw new SealError("This service has no ID a sealed value binds to.");
  return Uint8Array.from({ length: 16 }, (_, index) => Number.parseInt(hex.slice(index * 2, index * 2 + 2), 16));
}

function fromBase64(text: string) {
  return Uint8Array.from(atob(text), (char) => char.charCodeAt(0));
}

function toBase64(data: Uint8Array) {
  let binary = "";
  for (const byte of data) binary += String.fromCharCode(byte);
  return btoa(binary);
}
