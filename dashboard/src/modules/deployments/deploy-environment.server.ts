import "@tanstack/react-start/server-only";
import { servicePublicDomain } from "#/modules/environment-design/managed-service-exports";
import { Effect } from "effect";
import type { EnvironmentSnapshotVariableProducer } from "#/modules/environment-design/tables";
import { resolveVariables, type VariableProducer } from "@ployz/sdk/config";
import type { ServiceDeploymentConfig } from "#/modules/environment-design/services";
import type { SecretEncryptionService } from "#/utils/encrypted-secret.server";
import { Validation } from "#/server/public-error";

/**
 * Resolve a deployment's snapshot env to concrete strings at apply time: decrypt
 * sealed values and resolve `${{ }}` templates against the deployment's frozen
 * producers and public domains selected from its captured configuration.
 * Cycles fail; missing references resolve to "". Core's lowerDeployment
 * applies service defaults after this step, so authored values always take precedence.
 */
export const getResolvedDeployEnvBySnapshotConfig = Effect.fn(
  "Deployments.getResolvedDeployEnvBySnapshotConfig",
)(function* (
  encryption: SecretEncryptionService,
  snapshots: Array<{
    serviceId: string;
    config: Pick<ServiceDeploymentConfig, "env" | "routes" | "managedHostnames">;
  }>,
  frozenProducers: EnvironmentSnapshotVariableProducer[] | null,
  clusterDomain: string | null = null,
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

  const toProducer = (owner: EnvironmentSnapshotVariableProducer, key: string, value: VariableProducer["value"]): VariableProducer =>
    ({ ownerId: owner.ownerId, owner: { scope: "service", lineageId: owner.ownerLineageId }, key, value });
  const producers: VariableProducer[] = [];
  for (const snapshot of snapshots) {
    const domain = servicePublicDomain(snapshot.config, clusterDomain);
    if (!domain) continue;
    envByServiceId.set(snapshot.serviceId, { PLOYZ_PUBLIC_DOMAIN: domain });
    const owner = frozenProducers?.find((producer) => producer.ownerScope === "service" && producer.ownerId === snapshot.serviceId);
    if (owner) producers.push(toProducer(owner, "PLOYZ_PUBLIC_DOMAIN", { kind: "literal", value: domain }));
  }
  for (const frozen of frozenProducers ?? []) {
    const frozenValue = frozen.value;
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
    producers.push(toProducer(frozen, frozen.key, value));
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
            catch: (cause) => new Validation({ message: cause instanceof Error ? cause.message : "Variable resolution inputs are invalid." }),
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
