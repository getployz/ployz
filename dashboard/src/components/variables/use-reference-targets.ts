import { useClusterDomainName } from "#/modules/cluster-domain/use-cluster-domain-name";
import { useServicesCollection } from "#/modules/services/services.collection";
import { useLiveQuery, eq } from "@tanstack/react-db";
import { useEnvironmentDocument } from "#/modules/environment-design/environment-document.collection";
import { getManagedServiceExports } from "#/modules/environment-design/managed-service-exports";
import { buildReferenceTargets, type ReferenceTarget } from "#/modules/environment-design/variable-autocomplete";

export function useReferenceTargets(input: {
  organizationSlug: string;
  environmentId: string;
  serviceId: string;
}): ReferenceTarget[] {
  const clusterDomain = useClusterDomainName(input.organizationSlug);
  const document = useEnvironmentDocument(input.organizationSlug, input.environmentId);
  const services = useServicesCollection(input.organizationSlug);
  const { data: identities } = useLiveQuery({ queryKey: ['reference-services', services.id, input.environmentId], query: q => q.from({ service: services }).where(({ service }) => eq(service.environmentId, input.environmentId)) });
  const names = new Map(identities.map(service => [service.id, service.name]));
  if (!document) return [];
  return buildReferenceTargets({
    services: document.intent.services.map((service) => ({ slug: service.slug, name: names.get(service.id) ?? service.slug,
      isSelf: service.id === input.serviceId,
      variables: service.variables.map((variable) => ({
        key: variable.key, exported: variable.exported, isSecret: variable.value.kind === "secret", description: variable.description,
      })),
      managedExports: getManagedServiceExports({ ...service.config, name: names.get(service.id) ?? service.slug, id: service.id, lineageId: service.lineageId, slug: service.slug, environmentId: document.id, environmentSlug: document.namespace }, clusterDomain).map((exported) => ({ key: exported.key, description: exported.description })),
    })),
  });
}
