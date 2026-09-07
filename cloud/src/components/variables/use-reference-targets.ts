import { eq, toArray, useLiveSuspenseQuery } from "@tanstack/react-db";
import { parseLiveQueryRow } from "#/lib/tanstack-db";
import { getManagedServiceExports } from "#/modules/environment-design/managed-service-exports";
import { variableGroupResourceRecordSchema } from "#/modules/environment-design/resources";
import {
  buildReferenceTargets,
  type ReferenceTarget,
} from "#/modules/environment-design/variable-autocomplete";
import {
  useEnvironmentResourcesCollection,
  useServicesCollection,
  useVariablesCollection,
} from "#/modules/services/services.collection";

type ReferenceOwner =
  | { kind: "service"; serviceId: string }
  | { kind: "variable_group"; variableGroupId: string };

type OwnerVariableRow = {
  key: string;
  exported: boolean;
  description: string | null;
  value: { type: "plain" | "sealed" };
};

function toOwnerVariable(variable: OwnerVariableRow) {
  return {
    key: variable.key,
    exported: variable.exported,
    isSecret: variable.value.type === "sealed",
    description: variable.description,
  };
}

/**
 * Reference targets for the `${{ }}` autocomplete, derived live from the
 * environment's services + variable groups. Reactive: renaming or adding a
 * producer updates the suggestions without a refetch.
 */
export function useReferenceTargets(input: {
  organizationSlug: string;
  environmentId: string;
  owner: ReferenceOwner;
}): ReferenceTarget[] {
  const services = useServicesCollection(input.organizationSlug);
  const environmentResources = useEnvironmentResourcesCollection(
    input.organizationSlug,
  );
  const variables = useVariablesCollection(input.organizationSlug);

  const { data: serviceRows } = useLiveSuspenseQuery({
    query: (q) =>
      q
        .from({ service: services })
        .where(({ service }) => eq(service.environmentId, input.environmentId))
        .select(({ service }) => ({
          service,
          variables: toArray(
            q
              .from({ variable: variables })
              .where(({ variable }) => eq(variable.serviceId, service.id)),
          ),
        })),
  });

  const { data: resourceRows } = useLiveSuspenseQuery({
    query: (q) =>
      q
        .from({ resource: environmentResources })
        .where(({ resource }) =>
          eq(resource.resource.environmentId, input.environmentId),
        )
        .select(({ resource }) => resource),
  });
  const resources = resourceRows.map((resource) =>
    parseLiveQueryRow(variableGroupResourceRecordSchema, resource),
  );

  return buildReferenceTargets({
    ownerScope: input.owner.kind,
    services: serviceRows.map((row) => ({
      slug: row.service.slug,
      name: row.service.name,
      isSelf:
        input.owner.kind === "service" && row.service.id === input.owner.serviceId,
      variables: row.variables.map((variable) =>
        toOwnerVariable({
          key: variable.key,
          exported: variable.exported,
          description: variable.description,
          value: { type: variable.value.type },
        }),
      ),
      managedExports: getManagedServiceExports(row.service).map((exported) => ({
        key: exported.key,
        description: exported.description,
      })),
    })),
    variableGroups: resources.map((resource) => ({
      slug: resource.variableGroup.slug,
      name: resource.variableGroup.name,
      isSelf:
        input.owner.kind === "variable_group" &&
        resource.variableGroup.id === input.owner.variableGroupId,
      variables: resource.variables.map((variable) =>
        toOwnerVariable({
          key: variable.key,
          exported: variable.exported,
          description: variable.description,
          value: { type: variable.value.type },
        }),
      ),
    })),
  });
}
