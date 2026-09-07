import type {
  ServiceDeployEnv,
  ServiceDeployEnvValue,
} from "#/modules/environment-design/services";
import type { VariableRecord } from "#/modules/environment-design/variables";

type DeployEnvVariable = Pick<VariableRecord, "id" | "key" | "value">;
type DeployEnvValueSource = NonNullable<ServiceDeployEnvValue["source"]>;
type OrderedDeployEnvAttachment<TVariable extends { key: string }> = {
  sortOrder: number;
  variable: TVariable;
  source?: DeployEnvValueSource;
};

function compareKeyedVariables(left: { key: string }, right: { key: string }) {
  return left.key.localeCompare(right.key);
}

export function sortDeployEnvAttachments<
  TAttachment extends OrderedDeployEnvAttachment<{ key: string }>,
>(
  attachments: TAttachment[],
) {
  return [...attachments].sort((left, right) => {
    const orderDelta = left.sortOrder - right.sortOrder;
    if (orderDelta !== 0) return orderDelta;

    return compareKeyedVariables(left.variable, right.variable);
  });
}

function getDeployEnvFromVariables(
  variables: DeployEnvVariable[],
  source?: DeployEnvValueSource,
): ServiceDeployEnv {
  return Object.fromEntries(
    variables.map((variable) => [
      variable.key,
      variable.value.type === "plain"
        ? source
          ? {
              kind: "literal" as const,
              value: variable.value.value,
              source,
            }
          : {
              kind: "literal" as const,
              value: variable.value.value,
            }
        : source
          ? {
              kind: "secret" as const,
              variableId: variable.id,
              fingerprint: variable.value.fingerprint,
              source,
            }
          : {
              kind: "secret" as const,
              variableId: variable.id,
              fingerprint: variable.value.fingerprint,
            },
    ]),
  );
}

export function getDeployEnvFromServiceVariables(input: {
  inlineVariables: DeployEnvVariable[];
  variableGroupAttachments: {
    sortOrder: number;
    resourceId?: string;
    resourceName?: string;
    variableGroupId?: string;
    variables: (DeployEnvVariable & { exported: boolean })[];
  }[];
}): ServiceDeployEnv {
  const env: ServiceDeployEnv = {
    ...getDeployEnvFromVariables(
      [...input.inlineVariables].sort(compareKeyedVariables),
    ),
  };
  const attachments = sortDeployEnvAttachments(
    input.variableGroupAttachments.flatMap((attachment) =>
      attachment.variables.flatMap((variable) =>
        variable.exported
          ? [
              {
                sortOrder: attachment.sortOrder,
                variable,
                source:
                  attachment.resourceId &&
                  attachment.resourceName &&
                  attachment.variableGroupId
                    ? {
                        kind: "variable_group" as const,
                        resourceId: attachment.resourceId,
                        resourceName: attachment.resourceName,
                        variableGroupId: attachment.variableGroupId,
                        key: variable.key,
                      }
                    : undefined,
              },
            ]
          : [],
      ),
    ),
  );

  for (const attachment of attachments) {
    Object.assign(
      env,
      getDeployEnvFromVariables([attachment.variable], attachment.source),
    );
  }

  return env;
}
