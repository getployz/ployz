import type { service } from "./tables";
import type { SavedEnvironmentIntent } from "./saved-intent";
import { decodeStrict } from "./schema";
import { serviceSelectSchema, type ServiceRecord } from "./services";

export function serviceDocumentRecord(
  identity: typeof service.$inferSelect,
  node: SavedEnvironmentIntent["services"][number],
  registryCredentialUsername: string | null = null,
): ServiceRecord {
  const { version: _version, ...settings } = node.config;
  return decodeStrict(serviceSelectSchema, {
    ...settings, id: node.id, lineageId: node.lineageId, slug: node.slug,
    environmentId: identity.environmentId,
    registryCredentialUsername, hasStoredRegistryCredential: identity.hasRegistryCredential,
    firstDeployedAt: identity.firstDeployedAt, deletedAt: null,
    createdAt: identity.createdAt, updatedAt: identity.updatedAt,
  });
}

