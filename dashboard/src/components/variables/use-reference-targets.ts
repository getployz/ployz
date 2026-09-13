import { useEnvironmentDocument } from "#/modules/environment-design/environment-document.collection";
import { getManagedServiceExports } from "#/modules/environment-design/managed-service-exports";
import { buildReferenceTargets, type ReferenceTarget } from "#/modules/environment-design/variable-autocomplete";

type ReferenceOwner =
  | { kind: "service"; serviceId: string }
  | { kind: "variable_group"; variableGroupId: string };

export function useReferenceTargets(input: {
  organizationSlug: string;
  environmentId: string;
  owner: ReferenceOwner;
}): ReferenceTarget[] {
  const document = useEnvironmentDocument(input.organizationSlug, input.environmentId);
  if (!document) return [];
  const variables = (entries: typeof document.intent.services[number]["variables"]) => entries.map((variable) => ({
    key: variable.key, exported: variable.exported, isSecret: variable.value.kind === "secret", description: variable.description,
  }));
  return buildReferenceTargets({ ownerScope: input.owner.kind,
    services: document.intent.services.map((service) => ({ slug: service.slug, name: service.config.name,
      isSelf: input.owner.kind === "service" && service.id === input.owner.serviceId,
      variables: variables(service.variables),
      managedExports: getManagedServiceExports({ ...service.config, id: service.id, lineageId: service.lineageId, slug: service.slug, environmentId: document.id, environmentSlug: document.namespace }).map((exported) => ({ key: exported.key, description: exported.description })),
    })),
    variableGroups: document.intent.variableGroups.map((group) => ({ slug: group.slug, name: group.name,
      isSelf: input.owner.kind === "variable_group" && group.variableGroupId === input.owner.variableGroupId,
      variables: variables(group.variables),
    })),
  });
}
