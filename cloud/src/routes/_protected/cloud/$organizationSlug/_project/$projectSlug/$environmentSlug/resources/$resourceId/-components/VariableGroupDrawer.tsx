import { useServerFn } from "@tanstack/react-start";
import { getRawEnvironmentResourcesCollection } from "#/electric/collections";
import {
  Tabs,
  TabsContent,
  TabsList,
  TabsTrigger,
} from "#/components/ui/tabs";
import { createEnvironmentNodeNameSchema } from "#/modules/environment-design/environment-node-names";
import { environmentDesignFields } from "#/modules/environment-design/fields";
import { updateVariableGroupResourceServerFn } from "#/modules/environment-design/resource-functions";
import { CanvasInspectorHeader } from "#/routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/-components/CanvasInspectorHeader";
import { CanvasInspectorNameEditor } from "#/routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/-components/CanvasInspectorNameEditor";
import { VariableGroupVariablesTab } from "#/routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/resources/$resourceId/-components/VariableGroupVariablesTab";
import type {
  VariableGroupDrawerState,
  VariableGroupResourceRouteParams,
} from "#/routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/resources/$resourceId/-components/useVariableGroupDrawerState";

const resourceNameSchema = environmentDesignFields.resource.name;

export function VariableGroupDrawer({
  params,
  state,
}: {
  params: VariableGroupResourceRouteParams;
  state: VariableGroupDrawerState;
}) {
  const updateVariableGroup = useServerFn(updateVariableGroupResourceServerFn);
  const resourceId = state.resource.resource.id;
  const nameSchema = createEnvironmentNodeNameSchema({
    schema: resourceNameSchema,
    nodes: state.environmentNodes,
    excludeNode: {
      type: "variable_group",
      id: resourceId,
    },
  });

  return (
    <div className="flex h-full min-h-0 flex-col bg-background">
      <CanvasInspectorHeader params={params}>
        <CanvasInspectorNameEditor
          value={state.resource.resource.name}
          schema={nameSchema}
          editTitle="Edit Variable Group name"
          editDescription="Rename this Variable Group."
          placeholder="Variable Group name"
          onRename={async (value) => {
            const receipt = await updateVariableGroup({ data: {
              organizationSlug: state.organizationSlug,
              environmentId: state.resource.resource.environmentId,
              resourceId,
              name: value,
            } });
            await getRawEnvironmentResourcesCollection(
              state.organizationSlug,
            ).utils.awaitTxId(receipt.txid);
          }}
        />
        <p className="truncate text-sm text-muted-foreground">Variable Group</p>
      </CanvasInspectorHeader>
      <Tabs
        defaultValue="variables"
        className="flex min-h-0 flex-1 flex-col overflow-hidden px-6 pb-6"
      >
        <TabsList variant="line">
          <TabsTrigger value="variables">Variables</TabsTrigger>
        </TabsList>
        <TabsContent value="variables" className="mt-4 overflow-y-auto">
          <VariableGroupVariablesTab state={state} />
        </TabsContent>
      </Tabs>
    </div>
  );
}
