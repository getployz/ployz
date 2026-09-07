import "@tanstack/react-start/server-only";
import { Effect } from "effect";
import {
  adjectives,
  animals,
  uniqueNamesGenerator,
} from "unique-names-generator";
import type { Actor } from "#/modules/identity/actor";
import {
  getCustomDomainCapability,
  routeMutationRequiresCustomDomainCapability,
} from "#/modules/billing/custom-domain-capability";
import { withMutationReceipt } from "#/server/mutation-receipt.server";
import { Conflict, Forbidden, NotFound } from "#/server/public-error";
import { slugifySegment, allocateUnique } from "#/utils/slug";
import {
  SecretEncryption,
} from "#/utils/encrypted-secret.server";
import {
  createServiceLineage,
  listEnvironmentNodeNameIdentities,
  requireEnvironmentForActor,
  requireEnvironmentForActorById,
  requireOrganizationForActor,
} from "./authoring-repository.server";
import { environmentDesignFields } from "./fields";
import {
  getDuplicateEnvironmentNodeNameMessage,
  isEnvironmentNodeNameTaken,
  resolveUniqueEnvironmentNodeName,
} from "./environment-node-names";
import {
  deleteServiceRecords,
  deleteServiceLineage,
  exposedRegistryCredentialUsername,
  getServiceForOrganizationById,
  getServiceForUpdate,
  getStoredServiceCredential,
  insertCanvasPosition,
  insertService,
  listServicesForEnvironment,
  serviceExists,
  setStoredServiceCredential,
  updateServiceRecord,
  updateServiceSource,
  upsertCanvasPosition,
} from "./service-repository.server";
import {
  detectRegistryCredentialProvider,
  normalizeRegistryCredentialUsername,
  type ClearServiceRegistryCredentialInput,
  type CreateServiceInput,
  type RestoreServiceRegistryCredentialInput,
  type ServiceSource,
  type SetServiceRegistryCredentialInput,
  type UpdateServiceInput,
} from "./services";

function generateServiceName() {
  return uniqueNamesGenerator({
    dictionaries: [adjectives, animals],
    separator: "-",
    length: 2,
    style: "lowerCase",
  });
}

function resolveServiceName(input: CreateServiceInput) {
  if (input.name?.trim()) return input.name.trim();
  if (input.source.type === "git") {
    return input.source.repository.split("/").at(-1) ?? generateServiceName();
  }
  if (input.source.type === "image") {
    const image = input.source.image.split("/").at(-1) ?? input.source.image;
    return image.split(":")[0] || generateServiceName();
  }
  return generateServiceName();
}

function serviceBaseSlug(name: string) {
  return slugifySegment(name) || "service";
}

export const listEnvironmentServices = Effect.fn(
  "EnvironmentDesign.listEnvironmentServices",
)(function* (
  actor: Actor,
  input: {
    readonly organizationSlug: string;
    readonly projectSlug: string;
    readonly environmentSlug: string;
  },
) {
  const context = yield* requireEnvironmentForActor(actor, input);
  return yield* listServicesForEnvironment(context.environment.id, {
    projectSlug: context.project.slug,
    environmentSlug: context.environment.namespace,
  });
});

export const getServiceById = Effect.fn("EnvironmentDesign.getServiceById")(
  function* (
    actor: Actor,
    input: { readonly organizationSlug: string; readonly serviceId: string },
  ) {
    const organization = yield* requireOrganizationForActor(actor, input.organizationSlug);
    const service = yield* getServiceForOrganizationById(
      organization.id,
      input.serviceId,
    );
    if (service === null) {
      return yield* new NotFound({ message: "Service not found." });
    }
    return service;
  },
);

const requireCredentialAccess = Effect.fn(
  "EnvironmentDesign.requireServiceCredentialAccess",
)(function* (
  actor: Actor,
  input: {
    readonly organizationSlug: string;
    readonly environmentId: string;
    readonly serviceId: string;
  },
) {
  yield* requireEnvironmentForActorById(actor, input);
  const service = yield* getStoredServiceCredential(
    input.environmentId,
    input.serviceId,
  );
  if (service === null) {
    return yield* new NotFound({ message: "Service not found." });
  }
  return service;
});

export const setServiceRegistryCredential = Effect.fn(
  "EnvironmentDesign.setServiceRegistryCredential",
)(function* (actor: Actor, input: SetServiceRegistryCredentialInput) {
  const encryption = yield* SecretEncryption;
  const service = yield* requireCredentialAccess(actor, input);
  if (service.source.type !== "image") {
    return yield* new Conflict({
      message: "Service does not use a container image.",
    });
  }
  const provider = detectRegistryCredentialProvider(service.source.image);
  const existingUsername = exposedRegistryCredentialUsername(service, encryption);
  const username = normalizeRegistryCredentialUsername({
    provider,
    username: input.username ?? existingUsername,
  });
  const revision = new Date().toISOString();
  const encryptedRegistryUsername =
    username === null ? null : encryption.encrypt(username);
  const encryptedRegistrySecret = encryption.encrypt(input.secret.trim());
  const source: ServiceSource = {
    ...service.source,
    credentials: { type: "configured", revision },
  };
  const receipt = yield* withMutationReceipt(
    setStoredServiceCredential({
      serviceId: service.id,
      source,
      encryptedRegistryUsername,
      encryptedRegistrySecret,
    }),
  );
  if (receipt.data === null) {
    return yield* new NotFound({ message: "Service not found." });
  }
  return { ...receipt, data: receipt.data };
});

export const clearServiceRegistryCredential = Effect.fn(
  "EnvironmentDesign.clearServiceRegistryCredential",
)(function* (actor: Actor, input: ClearServiceRegistryCredentialInput) {
  const service = yield* requireCredentialAccess(actor, input);
  const source: ServiceSource =
    service.source.type === "image"
      ? { ...service.source, credentials: { type: "none" } }
      : service.source;
  const receipt = yield* withMutationReceipt(
    updateServiceSource(service.id, source),
  );
  if (receipt.data === null) {
    return yield* new NotFound({ message: "Service not found." });
  }
  return { ...receipt, data: receipt.data };
});

export const restoreServiceRegistryCredential = Effect.fn(
  "EnvironmentDesign.restoreServiceRegistryCredential",
)(function* (actor: Actor, input: RestoreServiceRegistryCredentialInput) {
  const service = yield* requireCredentialAccess(actor, input);
  if (service.source.type !== "image") {
    return yield* new Conflict({
      message: "Service does not use a container image.",
    });
  }
  if (service.encryptedRegistrySecret === null) {
    return yield* new Conflict({
      message: "No saved registry credentials to restore.",
    });
  }
  const source: ServiceSource = {
    ...service.source,
    credentials: { type: "configured", revision: new Date().toISOString() },
  };
  const receipt = yield* withMutationReceipt(
    updateServiceSource(service.id, source),
  );
  if (receipt.data === null) {
    return yield* new NotFound({ message: "Service not found." });
  }
  return { ...receipt, data: receipt.data };
});

export const createService = Effect.fn("EnvironmentDesign.createService")(
  function* (actor: Actor, input: CreateServiceInput) {
    const context = yield* requireEnvironmentForActorById(actor, input);
    const requestedName = resolveServiceName(input);
    const attemptedNames = yield* listEnvironmentNodeNameIdentities(
      input.environmentId,
    );
    return yield* withMutationReceipt(
      allocateUnique({
        tryAttempt: (attempt) =>
          Effect.gen(function* () {
            const name = resolveUniqueEnvironmentNodeName({
              name: requestedName,
              nodes: attemptedNames,
              schema: environmentDesignFields.service.name,
              maxLength: 64,
            });
            const slug = serviceBaseSlug(name);
            const lineage = yield* createServiceLineage({
              projectId: context.project.id,
              name,
              slug,
            });
            if (lineage === null) {
              attemptedNames.push({
                type: "service",
                id: `attempt-${attempt}`,
                name,
              });
              return null;
            }
            const service = yield* insertService({
              projectId: context.project.id,
              environmentId: input.environmentId,
              lineageId: lineage.id,
              name,
              slug,
              source: input.source,
              preDeployCommand: input.preDeployCommand,
              startCommand: input.startCommand,
              healthcheck: input.healthcheck,
              restartPolicy: input.restartPolicy,
            });
            if (service === null) {
              yield* deleteServiceLineage(lineage.id);
              attemptedNames.push({
                type: "service",
                id: `attempt-${attempt}`,
                name,
              });
              return null;
            }
            const canvasPosition = yield* insertCanvasPosition({
              environmentId: input.environmentId,
              resourceId: service.id,
              x: input.x,
              y: input.y,
            });
            if (canvasPosition === null) {
              return yield* Effect.die(
                "PostgreSQL did not return the service canvas position.",
              );
            }
            return {
              service: {
                ...service,
                projectSlug: context.project.slug,
                environmentSlug: context.environment.namespace,
              },
              canvasPosition,
            };
          }),
        exhausted: new Conflict({
          message: `Could not allocate a service name in ${input.environmentId}.`,
        }),
      }),
    );
  },
);

export const updateService = Effect.fn("EnvironmentDesign.updateService")(
  function* (actor: Actor, input: UpdateServiceInput) {
    const context = yield* requireEnvironmentForActorById(actor, input);
    const current = yield* getServiceForUpdate(input.environmentId, input.serviceId);
    if (current === null) {
      return yield* new NotFound({ message: "Service not found." });
    }
    const nextRoutes = input.routes ?? current.record.routes;
    if (routeMutationRequiresCustomDomainCapability(current.record.routes, nextRoutes)) {
      const capability = yield* getCustomDomainCapability(
        context.organization.id,
      );
      if (!capability.allowed) {
        return yield* new Forbidden({ message: capability.reason });
      }
    }
    const name = input.name ?? current.record.name;
    const source = input.source ?? current.record.source;
    const names = yield* listEnvironmentNodeNameIdentities(input.environmentId);
    if (
      isEnvironmentNodeNameTaken(name, names, {
        type: "service",
        id: current.record.id,
      })
    ) {
      return yield* new Conflict({
        message: getDuplicateEnvironmentNodeNameMessage(name),
      });
    }
    const receipt = yield* withMutationReceipt(
      updateServiceRecord(
        current.record.id,
        { ...input, name, source },
        {
          encryptedRegistryUsername: current.encryptedRegistryUsername,
          encryptedRegistrySecret: current.encryptedRegistrySecret,
        },
      ),
    );
    if (receipt.data === null) {
      return yield* new NotFound({ message: "Service not found." });
    }
    return { ...receipt, data: receipt.data };
  },
);

export const deleteServices = Effect.fn("EnvironmentDesign.deleteServices")(
  function* (
    actor: Actor,
    input: {
      readonly organizationSlug: string;
      readonly environmentId: string;
      readonly serviceIds: readonly string[];
    },
  ) {
    yield* requireEnvironmentForActorById(actor, input);
    return yield* withMutationReceipt(
      deleteServiceRecords(input.environmentId, input.serviceIds),
    );
  },
);

export const updateServiceCanvasPosition = Effect.fn(
  "EnvironmentDesign.updateServiceCanvasPosition",
)(function* (
  actor: Actor,
  input: {
    readonly organizationSlug: string;
    readonly environmentId: string;
    readonly serviceId: string;
    readonly x: number;
    readonly y: number;
  },
) {
  yield* requireEnvironmentForActorById(actor, input);
  if (!(yield* serviceExists(input.environmentId, input.serviceId))) {
    return yield* new NotFound({ message: "Service not found." });
  }
  return yield* withMutationReceipt(upsertCanvasPosition(input));
});
