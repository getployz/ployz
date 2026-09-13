import "@tanstack/react-start/server-only";
import { Effect } from "effect";
import { parseServiceConfig } from "@ployz/sdk/config";
import { captureEnvironmentNodeIntroduction } from "./environment-node-introduction.repository.server";
import { loadEnvironmentDocument, requireDocumentRevision, writeEnvironmentDocument } from "./working-state-repository.server";
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
import { withMutationResult } from "#/server/mutation-result.server";
import { Conflict, Forbidden, NotFound } from "#/server/public-error";
import { slugifySegment } from "#/utils/slug";
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
  exposedRegistryCredentialUsername,
  getServiceForOrganizationById,
  getStoredServiceCredential,
  insertCanvasPosition,
  insertServiceIdentity,
  serviceDocumentRecord,
  listServicesForEnvironment,
  serviceExists,
  upsertCanvasPosition,
} from "./service-repository.server";
import {
  detectRegistryCredentialProvider,
  normalizeRegistryCredentialUsername,
  type ClearServiceRegistryCredentialInput,
  type CreateServiceInput,
  type RestoreServiceRegistryCredentialInput,
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

const loadServiceEdit = Effect.fn("EnvironmentDesign.loadServiceEdit")(
  function* (input: { environmentId: string; serviceId: string; revision: string }) {
    const document = yield* loadEnvironmentDocument(input.environmentId, true);
    yield* requireDocumentRevision(document, input.revision);
    const node = document.intent.services.find((node) => node.id === input.serviceId);
    if (!node) return yield* new NotFound({ message: "Service not found." });
    return { document, node };
  },
);

export const setServiceRegistryCredential = Effect.fn("EnvironmentDesign.setServiceRegistryCredential")(
  function* (actor: Actor, input: SetServiceRegistryCredentialInput) {
    yield* requireEnvironmentForActorById(actor, input);
    const encryption = yield* SecretEncryption;
    return yield* withMutationResult(Effect.gen(function* () {
      const { document, node } = yield* loadServiceEdit(input);
      if (node.config.source.type !== "image") return yield* new Conflict({ message: "Service does not use a container image." });
      const stored = yield* getStoredServiceCredential(input.environmentId, input.serviceId);
      const username = normalizeRegistryCredentialUsername({
        provider: detectRegistryCredentialProvider(node.config.source.image),
        username: input.username ?? (stored ? exposedRegistryCredentialUsername(stored, encryption) : null),
      });
      node.encryptedRegistryUsername = username === null ? null : encryption.encrypt(username);
      node.encryptedRegistrySecret = encryption.encrypt(input.secret.trim());
      node.config.source.credentials = { type: "configured", revision: new Date().toISOString() };
      return yield* writeEnvironmentDocument(document, document.intent);
    }));
  },
);

export const clearServiceRegistryCredential = Effect.fn("EnvironmentDesign.clearServiceRegistryCredential")(
  function* (actor: Actor, input: ClearServiceRegistryCredentialInput) {
    yield* requireEnvironmentForActorById(actor, input);
    return yield* withMutationResult(Effect.gen(function* () {
      const { document, node } = yield* loadServiceEdit(input);
      if (node.config.source.type === "image") node.config.source.credentials = { type: "none" };
      return yield* writeEnvironmentDocument(document, document.intent);
    }));
  },
);

export const restoreServiceRegistryCredential = Effect.fn("EnvironmentDesign.restoreServiceRegistryCredential")(
  function* (actor: Actor, input: RestoreServiceRegistryCredentialInput) {
    yield* requireEnvironmentForActorById(actor, input);
    return yield* withMutationResult(Effect.gen(function* () {
      const { document, node } = yield* loadServiceEdit(input);
      if (node.config.source.type !== "image") return yield* new Conflict({ message: "Service does not use a container image." });
      const stored = yield* getStoredServiceCredential(input.environmentId, input.serviceId);
      if (!stored?.encryptedRegistrySecret) return yield* new Conflict({ message: "No saved registry credentials to restore." });
      node.config.source.credentials = { type: "configured", revision: new Date().toISOString() };
      return yield* writeEnvironmentDocument(document, document.intent);
    }));
  },
);

export const createService = Effect.fn("EnvironmentDesign.createService")(
  function* (actor: Actor, input: CreateServiceInput) {
    const context = yield* requireEnvironmentForActorById(actor, input);
    return yield* withMutationResult(Effect.gen(function* () {
      const document = yield* loadEnvironmentDocument(input.environmentId, true);
      const name = resolveUniqueEnvironmentNodeName({ name: resolveServiceName(input),
        nodes: yield* listEnvironmentNodeNameIdentities(input.environmentId),
        schema: environmentDesignFields.service.name, maxLength: 64 });
      const slug = serviceBaseSlug(name);
      const lineage = yield* createServiceLineage({ projectId: context.project.id, name, slug });
      if (!lineage) return yield* new Conflict({ message: "Could not allocate a service lineage." });
      const identity = yield* insertServiceIdentity({ projectId: context.project.id, environmentId: input.environmentId, lineageId: lineage.id });
      const { env: _env, mounts: _mounts, variableGroupAttachments: _variableGroupAttachments, ...config } = parseServiceConfig({ version: 2, name, source: input.source,
        preDeployCommand: input.preDeployCommand, startCommand: input.startCommand,
        healthcheck: input.healthcheck, restartPolicy: input.restartPolicy, privateDns: slug });
      const node = { id: identity.id, lineageId: lineage.id, slug, config, variables: [], variableGroupAttachments: [], volumeAttachments: [], encryptedRegistryUsername: null, encryptedRegistrySecret: null };
      document.intent.services.push(node);
      const environment = yield* writeEnvironmentDocument(document, document.intent);
      const introduction = yield* captureEnvironmentNodeIntroduction({ environmentId: input.environmentId, nodeType: "service", nodeId: identity.id });
      const canvasPosition = yield* insertCanvasPosition({ environmentId: input.environmentId, resourceId: identity.id, x: input.x, y: input.y });
      return { service: { ...serviceDocumentRecord(identity, node), projectSlug: context.project.slug, environmentSlug: context.environment.namespace }, canvasPosition, environment, identity, introduction };
    }));
  },
);

export const updateService = Effect.fn("EnvironmentDesign.updateService")(
  function* (actor: Actor, input: UpdateServiceInput) {
    const context = yield* requireEnvironmentForActorById(actor, input);
    return yield* withMutationResult(Effect.gen(function* () {
      const { document, node } = yield* loadServiceEdit(input);
      if (routeMutationRequiresCustomDomainCapability(node.config.routes, input.routes ?? node.config.routes)) {
        const capability = yield* getCustomDomainCapability(context.organization.id);
        if (!capability.allowed) return yield* new Forbidden({ message: capability.reason });
      }
      const name = input.name ?? node.config.name;
      if (isEnvironmentNodeNameTaken(name, yield* listEnvironmentNodeNameIdentities(input.environmentId), { type: "service", id: node.id })) {
        return yield* new Conflict({ message: getDuplicateEnvironmentNodeNameMessage(name) });
      }
      const { organizationSlug: _organization, environmentId: _environment, serviceId: _service, revision: _revision, deletedAt, ...settings } = input;
      if (deletedAt) document.intent.services = document.intent.services.filter((candidate) => candidate.id !== node.id);
      else Object.assign(node.config, settings);
      return yield* writeEnvironmentDocument(document, document.intent);
    }));
  },
);

export const deleteServices = Effect.fn("EnvironmentDesign.deleteServices")(
  function* (actor: Actor, input: { readonly organizationSlug: string; readonly environmentId: string; readonly revision: string; readonly serviceIds: readonly string[] }) {
    yield* requireEnvironmentForActorById(actor, input);
    return yield* withMutationResult(Effect.gen(function* () {
      const document = yield* loadEnvironmentDocument(input.environmentId, true);
      yield* requireDocumentRevision(document, input.revision);
      document.intent.services = document.intent.services.filter((node) => !input.serviceIds.includes(node.id));
      return yield* writeEnvironmentDocument(document, document.intent);
    }));
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
  return yield* withMutationResult(upsertCanvasPosition(input));
});
