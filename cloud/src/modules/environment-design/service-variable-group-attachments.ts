import type { VariableGroupResourceRecord } from "#/modules/environment-design/resources";
import type {
  EnvironmentServiceVariableGroupAttachment,
  EnvironmentVariableGroupRecord,
} from "#/modules/environment-design/variables";

export type ServiceVariableGroupAttachmentAvailability = {
  attachedVariableGroups: EnvironmentVariableGroupRecord[];
  availableVariableGroups: EnvironmentVariableGroupRecord[];
};

export function getServiceVariableGroupAttachmentAvailability(input: {
  attachments: EnvironmentServiceVariableGroupAttachment[];
  environmentResources: VariableGroupResourceRecord[];
}): ServiceVariableGroupAttachmentAvailability {
  const attachedSetIds = new Set(
    input.attachments.map((attachment) => attachment.variableGroupId),
  );

  return {
    attachedVariableGroups: input.environmentResources.flatMap((resource) =>
      attachedSetIds.has(resource.variableGroup.id) ? [resource.variableGroup] : [],
    ),
    availableVariableGroups: input.environmentResources.flatMap((resource) =>
      attachedSetIds.has(resource.variableGroup.id) ? [] : [resource.variableGroup],
    ),
  };
}
