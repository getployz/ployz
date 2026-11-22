import { Effect, Layer, Redacted } from "effect";
import { AppConfig } from "#/server/config.server";
import { makeSecretEncryption, SecretEncryption } from "#/utils/encrypted-secret.server";

export const SecretEncryptionLive = Layer.effect(
  SecretEncryption,
  Effect.map(AppConfig, (config) => makeSecretEncryption(Redacted.value(config.encryptionSecret))),
);
