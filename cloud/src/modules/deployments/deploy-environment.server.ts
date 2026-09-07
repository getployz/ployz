import "@tanstack/react-start/server-only";
import { Effect, Result } from "effect";
import type { EnvironmentSnapshotVariableProducer } from "#/modules/environment-design/tables";
import type { ServiceDeploymentConfig } from "#/modules/environment-design/services";
import {
  resolveValueParts,
  type ProducerLookup,
} from "#/modules/environment-design/variable-resolution";
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
    snapshots.map((snapshot) => [snapshot.serviceId, {}]),
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

  const decryptedFrozenSecrets = new Map<string, string>();
  for (const producer of frozenProducers ?? []) {
    if (producer.value.kind !== "secret") continue;
    const encryptedValue = producer.value.encryptedValue;
    const plaintext = yield* Effect.try({
      try: () => encryption.decrypt(encryptedValue),
      catch: (cause) =>
        new Validation({
          message:
            cause instanceof Error
              ? cause.message
              : "Secret variable could not be decrypted.",
        }),
    });
    decryptedFrozenSecrets.set(
      `${producer.ownerId}:${producer.key}`,
      plaintext,
    );
  }

  const producersByOwner = new Map<
    string,
    Map<string, EnvironmentSnapshotVariableProducer>
  >();
  for (const producer of frozenProducers ?? []) {
    const owner = producersByOwner.get(producer.ownerId) ?? new Map();
    owner.set(producer.key, producer);
    producersByOwner.set(producer.ownerId, owner);
  }
  const serviceIdByLineage = new Map(
    (frozenProducers ?? []).flatMap((producer) =>
      producer.ownerScope === "service"
        ? [[producer.ownerLineageId, producer.ownerId] as const]
        : [],
    ),
  );
  const variableGroupIdByLineage = new Map(
    (frozenProducers ?? []).flatMap((producer) =>
      producer.ownerScope === "variable_group"
        ? [[producer.ownerLineageId, producer.ownerId] as const]
        : [],
    ),
  );
  const lookup: ProducerLookup = ({ owner, selfOwnerId, key }) => {
    const ownerId =
      owner.scope === "self"
        ? selfOwnerId
        : owner.scope === "service"
          ? serviceIdByLineage.get(owner.lineageId)
          : variableGroupIdByLineage.get(owner.lineageId);
    if (!ownerId) return null;
    const frozen = producersByOwner.get(ownerId)?.get(key);
    if (!frozen) return null;
    if (frozen.value.kind === "secret") {
      const plaintext = decryptedFrozenSecrets.get(`${ownerId}:${key}`);
      if (plaintext === undefined) return null;
      return {
        ownerId,
        producer: { kind: "secret" as const, value: plaintext },
      };
    }
    return { ownerId, producer: frozen.value };
  };

  for (const snapshot of snapshots) {
    const env = envByServiceId.get(snapshot.serviceId);
    if (!env) {
      continue;
    }

    for (const [key, value] of Object.entries(snapshot.config.env)) {
      if (value.kind === "literal") {
        if (value.parts) {
          const resolved = resolveValueParts({
            parts: value.parts,
            selfOwnerId: snapshot.serviceId,
            lookup,
          });
          if (Result.isFailure(resolved)) {
            return yield* resolved.failure;
          }
          env[key] = resolved.success.value;
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
