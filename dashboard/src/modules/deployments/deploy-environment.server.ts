import "@tanstack/react-start/server-only";
import { Effect } from "effect";
import type { EnvironmentSnapshotVariableProducer } from "#/modules/environment-design/tables";
import { resolveVariableParts, type ResolvedVariableProducer } from "#/modules/environment-design/variable-resolution";
import type { ServiceDeploymentConfig } from "#/modules/environment-design/services";
import type { SecretEncryptionService } from "#/utils/encrypted-secret.server";
import { Validation } from "#/server/public-error";

/**
 * Resolve a deployment's snapshot env to concrete strings at apply time: decrypt
 * sealed values and resolve `${{ }}` templates against the environment's current
 * producers. Cycles fail; missing references resolve to "".
 */
export const getResolvedDeployEnvBySnapshotConfig = Effect.fn(
  "Deployments.getResolvedDeployEnvBySnapshotConfig",
)(function* (
  encryption: SecretEncryptionService,
  snapshots: Array<{
    serviceId: string;
    config: Pick<ServiceDeploymentConfig, "env">;
  }>,
  frozenProducers: EnvironmentSnapshotVariableProducer[] | null,
) {
  const envByServiceId = new Map<string, Record<string, string>>(
    snapshots.map((snapshot) => [snapshot.serviceId, { PORT: "8080" }]),
  );

  const hasTemplates = snapshots.some((snapshot) =>
    Object.values(snapshot.config.env).some(
      (value) => value.kind === "literal" && value.parts,
    ),
  );
  if (hasTemplates && frozenProducers === null) {
    return yield* new Validation({
      message:
        "Templated deployment snapshot is missing frozen variable producers.",
    });
  }

  const producers: ResolvedVariableProducer[] = [];
  for (const producer of frozenProducers ?? []) {
    const frozenValue = producer.value;
    const value = frozenValue.kind === "secret"
      ? {
          kind: "secret" as const,
          value: yield* Effect.try({
            try: () => {
              if (!frozenValue.encryptedValue) throw new Error("Frozen secret is missing.");
              return encryption.decrypt(frozenValue.encryptedValue);
            },
            catch: () => new Validation({ message: "Secret variable could not be decrypted." }),
          }),
        }
      : frozenValue;
    producers.push({
      ...producer,
      value,
    });
  }

  for (const snapshot of snapshots) {
    const env = envByServiceId.get(snapshot.serviceId);
    if (!env) {
      continue;
    }

    for (const [key, value] of Object.entries(snapshot.config.env)) {
      if (value.kind === "literal") {
        if (value.parts) {
          const parts = value.parts;
          const resolved = yield* Effect.try({
            try: () => resolveVariableParts(parts, snapshot.serviceId, producers),
            catch: (cause) => new Validation({ message: cause instanceof Error ? cause.message : "Variable resolution inputs are invalid." }),
          });
          env[key] = resolved;
        } else {
          env[key] = value.value;
        }
        continue;
      }

      if (!value.encryptedValue) {
        return yield* new Validation({
          message: `Secret variable ${key} is missing from the deployment snapshot.`,
        });
      }

      const encryptedValue = value.encryptedValue;
      env[key] = yield* Effect.try({
        try: () => encryption.decrypt(encryptedValue),
        catch: (cause) =>
          new Validation({
            message:
              cause instanceof Error
                ? cause.message
                : `Secret variable ${key} could not be decrypted.`,
          }),
      });
    }
  }

  return envByServiceId;
});
