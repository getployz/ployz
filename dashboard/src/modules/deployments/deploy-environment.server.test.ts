import { Effect } from "effect";
import { expect, it } from "vitest";
import { getResolvedDeployEnvBySnapshotConfig } from "./deploy-environment.server";
import { makeSecretEncryption } from "#/utils/encrypted-secret.server";

it("defaults PORT while preserving explicit literal and secret values", async () => {
  const encryption = makeSecretEncryption("test-encryption-secret");
  const env = await Effect.runPromise(getResolvedDeployEnvBySnapshotConfig(
    encryption,
    [
      { serviceId: "default", config: { env: {} } },
      { serviceId: "explicit", config: { env: { PORT: { kind: "literal", value: "80" } } } },
      { serviceId: "empty", config: { env: { PORT: { kind: "literal", value: "" } } } },
      { serviceId: "secret", config: { env: { PORT: { kind: "secret", fingerprint: encryption.sealedFingerprint("3000"), encryptedValue: encryption.encrypt("3000") } } } },
    ],
    null,
  ));
  expect(Object.fromEntries(env)).toEqual({
    default: { PORT: "8080" },
    explicit: { PORT: "80" },
    empty: { PORT: "" },
    secret: { PORT: "3000" },
  });
});
