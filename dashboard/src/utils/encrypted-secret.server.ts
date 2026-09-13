import "@tanstack/react-start/server-only";
import crypto from "node:crypto";
import { Context, Effect, Layer, Redacted } from "effect";
import type { EncryptedSecretValue } from "#/db/tables";
import { AppConfig } from "#/server/config.server";

const variableValueFingerprintVersion = "v1";
const variableValueFingerprintKeyPurpose =
  "ployz:variable-value-fingerprint:v1";
const sealedVariableValueFingerprintPurpose =
  "ployz:sealed-variable-value:v1";

function getEncryptionKey(encryptionSecret: string) {
  return crypto
    .createHash("sha256")
    .update(encryptionSecret)
    .digest();
}

function getVariableValueFingerprintKey(encryptionSecret: string) {
  return crypto
    .createHash("sha256")
    .update(variableValueFingerprintKeyPurpose)
    .update("\0")
    .update(encryptionSecret)
    .digest();
}

export function getPlainVariableValueFingerprint(value: string) {
  return `${variableValueFingerprintVersion}:${crypto
    .createHash("sha256")
    .update(JSON.stringify({ kind: "plain", value }))
    .digest("hex")}`;
}

export function getSealedVariableValueFingerprint(
  encryptionSecret: string,
  value: string,
) {
  return `${variableValueFingerprintVersion}:${crypto
    .createHmac("sha256", getVariableValueFingerprintKey(encryptionSecret))
    .update(sealedVariableValueFingerprintPurpose)
    .update("\0")
    .update(value, "utf8")
    .digest("hex")}`;
}

export function encryptSecretValue(
  encryptionSecret: string,
  value: string,
): EncryptedSecretValue {
  const iv = crypto.randomBytes(12);
  const cipher = crypto.createCipheriv(
    "aes-256-gcm",
    getEncryptionKey(encryptionSecret),
    iv,
  );
  const ciphertext = Buffer.concat([
    cipher.update(value, "utf8"),
    cipher.final(),
  ]);

  return {
    version: 1,
    iv: iv.toString("base64"),
    tag: cipher.getAuthTag().toString("base64"),
    ciphertext: ciphertext.toString("base64"),
  };
}

export function decryptSecretValue(
  encryptionSecret: string,
  value: EncryptedSecretValue,
) {
  const decipher = crypto.createDecipheriv(
    "aes-256-gcm",
    getEncryptionKey(encryptionSecret),
    Buffer.from(value.iv, "base64"),
  );
  decipher.setAuthTag(Buffer.from(value.tag, "base64"));

  return Buffer.concat([
    decipher.update(Buffer.from(value.ciphertext, "base64")),
    decipher.final(),
  ]).toString("utf8");
}

export interface SecretEncryptionService {
  readonly encrypt: (value: string) => EncryptedSecretValue;
  readonly decrypt: (value: EncryptedSecretValue) => string;
  readonly sealedFingerprint: (value: string) => string;
}

export function makeSecretEncryption(
  encryptionSecret: string,
): SecretEncryptionService {
  return {
    encrypt: (value) => encryptSecretValue(encryptionSecret, value),
    decrypt: (value) => decryptSecretValue(encryptionSecret, value),
    sealedFingerprint: (value) =>
      getSealedVariableValueFingerprint(encryptionSecret, value),
  };
}

export class SecretEncryption extends Context.Service<
  SecretEncryption,
  SecretEncryptionService
>()("ployz/SecretEncryption") {}

export const SecretEncryptionLive = Layer.effect(
  SecretEncryption,
  Effect.map(AppConfig, (config) =>
    makeSecretEncryption(Redacted.value(config.encryptionSecret)),
  ),
);
