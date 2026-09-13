import "@tanstack/react-start/server-only";
import { Effect } from "effect";
import { resolveVariables, type VariableProducer } from "@ployz/sdk/config";
import type { EnvironmentSnapshotVariableProducer } from "#/modules/environment-design/tables";
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

  const producers: VariableProducer[] = [];
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
      ownerId: producer.ownerId,
      owner: { scope: producer.ownerScope, lineageId: producer.ownerLineageId },
      key: producer.key,
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
            try: () => resolveVariables({ parts, selfOwnerId: snapshot.serviceId, producers }),
            catch: () => new Validation({ message: "Variable resolution inputs are invalid." }),
          });
          if (resolved.status === "cycle") {
            return yield* new Validation({ message: `Circular variable reference: ${resolved.path.join(" -> ")}` });
          }
          env[key] = resolved.value;
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
