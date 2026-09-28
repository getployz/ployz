import type { service, serviceRegistryCredential } from "#/modules/environment-design/tables";

type ServiceRow = typeof service.$inferSelect;
type CredentialRow = typeof serviceRegistryCredential.$inferSelect;
type Position = { resourceType: string; x: number; y: number };

/**
 * What nodes arriving from an Environment copy from their lineage's node there: display name, Deployment Policy,
 * registry credential (ciphertext) and canvas position. Read up front, so a copy can land after that Environment is gone.
 */
export type IdentitySources = {
  services: Array<{
    lineageId: string; name: string; policy: ServiceRow["policy"]; hasRegistryCredential: boolean;
    credential: { username: CredentialRow["encryptedRegistryUsername"]; secret: CredentialRow["encryptedRegistrySecret"] } | null;
    position: Position | null;
  }>;
  volumes: Array<{ lineageId: string; position: Position | null }>;
};
