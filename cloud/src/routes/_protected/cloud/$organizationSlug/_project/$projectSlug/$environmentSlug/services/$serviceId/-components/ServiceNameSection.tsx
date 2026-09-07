import { CollectionFieldInput } from "#/components/stageable/collection-field-input";
import {
  createEnvironmentNodeNameSchema,
} from "#/modules/environment-design/environment-node-names";
import { environmentDesignFields } from "#/modules/environment-design/fields";
import {
  Field,
  FieldDescription,
  FieldGroup,
  FieldLabel,
} from "#/components/ui/field";
import type { ServiceDrawerState } from "#/routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/services/$serviceId/-components/useServiceDrawerState";

export function ServiceNameSection({
  state,
}: {
  state: ServiceDrawerState;
}) {
  const { service, collection } = state;
  const nameSchema = createEnvironmentNodeNameSchema({
    schema: environmentDesignFields.service.name,
    nodes: state.environmentNodes,
    excludeNode: {
      type: "service",
      id: service.id,
    },
  });

  return (
    <FieldGroup>
      <Field>
        <FieldLabel htmlFor="service-name">Name</FieldLabel>
        <CollectionFieldInput
          resource="service"
          collection={collection}
          entity={service}
          entityId={service.id}
          path="name"
          label="Name"
          schema={nameSchema}
        />
        <FieldDescription>
          This label is used in the environment and throughout the project.
        </FieldDescription>
      </Field>
    </FieldGroup>
  );
}
