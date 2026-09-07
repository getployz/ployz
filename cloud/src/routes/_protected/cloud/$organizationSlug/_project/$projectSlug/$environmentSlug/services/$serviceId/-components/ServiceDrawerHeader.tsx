import { collectionFieldResources } from "#/components/stageable/collection-field-resources";
import { CanvasInspectorHeader } from "#/routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/-components/CanvasInspectorHeader";
import { CanvasInspectorNameEditor } from "#/routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/-components/CanvasInspectorNameEditor";
import { createEnvironmentNodeNameSchema } from "#/modules/environment-design/environment-node-names";
import { environmentDesignFields } from "#/modules/environment-design/fields";
import type {
  ServiceDrawerState,
  ServiceRouteParams,
} from "#/routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/services/$serviceId/-components/useServiceDrawerState";

const serviceNameSchema = environmentDesignFields.service.name;

export function ServiceDrawerHeader({
  params,
  state,
}: {
  params: ServiceRouteParams;
  state: ServiceDrawerState;
}) {
  const { service } = state;
  const nameSchema = createEnvironmentNodeNameSchema({
    schema: serviceNameSchema,
    nodes: state.environmentNodes,
    excludeNode: {
      type: "service",
      id: service.id,
    },
  });

  return (
    <CanvasInspectorHeader params={params}>
      <CanvasInspectorNameEditor
        value={service.name}
        schema={nameSchema}
        editTitle="Edit service name"
        editDescription="Rename this service."
        placeholder="Service name"
        onRename={async (value) => {
          await collectionFieldResources.service.commit({
            collection: state.collection,
            entityId: service.id,
            path: "name",
            value,
          }).isPersisted.promise;
        }}
      />
    </CanvasInspectorHeader>
  );
}
