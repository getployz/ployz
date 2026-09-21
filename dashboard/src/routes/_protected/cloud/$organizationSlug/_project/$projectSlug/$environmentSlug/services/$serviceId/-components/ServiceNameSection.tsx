import { SchemaFieldInput } from "#/components/stageable/schema-field-input";
import {
  createEnvironmentNodeNameSchema,
} from "#/modules/environment-design/environment-node-names";
import { environmentDesignFields } from "#/modules/environment-design/fields";
import {
  Field,
  FieldGroup,
  FieldLabel,
} from "#/components/ui/field";
import type { ServiceDrawerState } from "#/routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/services/$serviceId/-components/useServiceDrawerState";

export function ServiceNameSection({
  state,
}: {
  state: ServiceDrawerState;
}) {
  const { service } = state;
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
        <SchemaFieldInput
          value={service.name}
          isChanged={false}
          onCommit={(name) => state.editMetadata({ environmentId: service.environmentId, serviceId: service.id, edit: { kind: "rename", name } })}
          label="Name"
          schema={nameSchema}
        />
      </Field>
    </FieldGroup>
  );
}
