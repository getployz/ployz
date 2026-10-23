import { CipherSuite } from "@hpke/core";
import { Chacha20Poly1305 } from "@hpke/chacha20poly1305";
import { DhkemX25519HkdfSha256, HkdfSha256 } from "@hpke/dhkem-x25519";
import { afterEach, describe, expect, it, vi } from "vitest";
import { SealError } from "#/modules/sealing/seal-error";
import { sealValue, sealedValueProblem } from "./seal.client";

// Test keys only. ployzd's `sealing_key.rs` opens `CIPHERTEXT` from the same literals, so both sides stay compatible.
const RECIPIENT_PRIVATE_KEY = "347ace27753bb0420141f071765286281c5fb4c450a9811d4ab13b67ac3da146";
const RECIPIENT_PUBLIC_KEY = "NdiV1QY9P3ZyoC82YA/FaBs0zlWTG/CXU1tLfcS0CDg=";
const EPHEMERAL_IKM = "5ea515325cb640dfbf7379020181b41cf280fdf2c1eed56f496ecbc10670c153";
const ORGANIZATION = "org_2xVq8LmN4rTb";
const ENVIRONMENT = "0e7c2f3a-5b1d-4c8e-9f60-1a2b3c4d5e6f";
const SERVICE = "7d4e9a12-3c5b-4f6e-8a90-b1c2d3e4f5a6";
const KEY = "SIGNING_KEY";
const PLAINTEXT = "línea uno 🔐\nzweite Zeile ✓\n";
const AAD =
  "0000000f706c6f797a2d7365616c65642d7631000000106f72675f32785671384c6d4e34725462000000100e7c2f3a5b1d4c8e9f601a2b3c4d5e6f0000000773657276696365000000107d4e9a123c5b4f6e8a90b1c2d3e4f5a60000000b5349474e494e475f4b4559";
const CIPHERTEXT =
  "aQ3JpLLRSRFxTny/uywru4DsdBNaz2ed/8puzj80FnXxzlL2DR0xHieAOAisLAf2fr0pN6dractyCf7l0M6K2KvTlZZRDfI84Nb+Oja16vnS";

const recipient = { organizationId: ORGANIZATION, publicKey: RECIPIENT_PUBLIC_KEY };
const target = { environment: ENVIRONMENT, service: SERVICE };
const suite = new CipherSuite({ kem: new DhkemX25519HkdfSha256(), kdf: new HkdfSha256(), aead: new Chacha20Poly1305() });

function hex(text: string) {
  return Uint8Array.from(text.match(/../g) ?? [], (pair) => Number.parseInt(pair, 16)).buffer;
}

async function open(sealed: string, aad: ArrayBuffer, privateKey = RECIPIENT_PRIVATE_KEY) {
  const bytes = Uint8Array.from(atob(sealed), (char) => char.charCodeAt(0));
  const opened = await suite.open(
    {
      recipientKey: await suite.kem.deserializePrivateKey(hex(privateKey)),
      enc: bytes.slice(0, 32).buffer,
      info: new TextEncoder().encode("ployz-sealed-v1").buffer,
    },
    bytes.slice(32).buffer,
    aad,
  );
  return new TextDecoder().decode(opened);
}

describe("sealValue", () => {
  afterEach(() => {
    vi.restoreAllMocks();
  });

  it("seals the frozen vector byte for byte", async () => {
    const createSenderContext = CipherSuite.prototype.createSenderContext;
    vi.spyOn(CipherSuite.prototype, "createSenderContext").mockImplementation(function (this: CipherSuite, params) {
      return createSenderContext.call(this, { ...params, ekm: hex(EPHEMERAL_IKM) });
    });

    const sealed = await sealValue(recipient, target, KEY, PLAINTEXT);

    expect(sealed).toBe(CIPHERTEXT);
    expect(await open(sealed, hex(AAD))).toBe(PLAINTEXT);
  });

  it("binds the value to its Organization, Environment, Service and key", async () => {
    expect(await open(await sealValue(recipient, target, KEY, PLAINTEXT), hex(AAD))).toBe(PLAINTEXT);
    for (const sealed of [
      sealValue({ ...recipient, organizationId: "org_other" }, target, KEY, PLAINTEXT),
      sealValue(recipient, { ...target, environment: SERVICE }, KEY, PLAINTEXT),
      sealValue(recipient, { ...target, service: ENVIRONMENT }, KEY, PLAINTEXT),
      sealValue(recipient, target, "OTHER_KEY", PLAINTEXT),
    ]) {
      await expect(open(await sealed, hex(AAD))).rejects.toThrow();
    }
  });

  it("seals the same value differently each time", async () => {
    expect(await sealValue(recipient, target, KEY, PLAINTEXT)).not.toBe(await sealValue(recipient, target, KEY, PLAINTEXT));
  });

  it("refuses what the browser can't seal before loading anything", async () => {
    await expect(sealValue(recipient, target, "PORT", "8080")).rejects.toBeInstanceOf(SealError);
    await expect(sealValue(recipient, { ...target, service: "web" }, KEY, PLAINTEXT)).rejects.toBeInstanceOf(SealError);
  });
});

describe("sealedValueProblem", () => {
  it.each([
    ["one line shorter than 6 bytes", "abc", null],
    ["a 5-byte row among longer ones", "abcdef\nabcde\n", "Each line of a multiline sealed value needs at least 6 characters."],
    ["a 1-byte row", "abcdef\na", "Each line of a multiline sealed value needs at least 6 characters."],
    ["6-byte rows", "abcdef\nghijkl", null],
    ["a trailing LF starts no row", "abc\n", null],
    ["an empty interior row", "abcdef\n\nghijkl", null],
    ["CR counts toward its row", "abcde\r\nfghij\r\n", null],
    ["a short row with CR", "abcd\r\nfghijk", "Each line of a multiline sealed value needs at least 6 characters."],
    ["multi-byte characters count in bytes", "ééé\nabcdef", null],
    ["two 2-byte characters are 4 bytes", "éé\nabcdef", "Each line of a multiline sealed value needs at least 6 characters."],
    ["a reference", "${{ db.URL }}", "A sealed value is stored as-is. Untick Sealed to use a reference."],
    ["a null character", "abc\0def", "A value can't hold a null character."],
    ["an empty value", "", "A sealed value can't be empty."],
    ["an unpaired surrogate", "abc\uD800def", "A value can't hold an unpaired surrogate."],
    ["a surrogate pair", "abc😀def", null],
  ])("%s", (_, value, problem) => {
    expect(sealedValueProblem(KEY, value)).toBe(problem);
  });

  it("refuses PORT", () => {
    expect(sealedValueProblem("PORT", "8080")).toBe("PORT can't be sealed: Ployz reads it to route traffic.");
  });

  it("refuses a value at the Store's length limit", () => {
    const limit = 128 * 1024 - KEY.length - 2;
    expect(sealedValueProblem(KEY, "a".repeat(limit))).toBeNull();
    expect(sealedValueProblem(KEY, "a".repeat(limit + 1))).toBe("Too long: a variable holds at most 128 KiB, name included.");
  });
});
