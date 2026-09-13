import { FieldGroup } from "#/components/ui/field";
import { SERVICE_DEPLOYMENT_DIFF_PATHS } from "#/modules/services/service-deployment-diff/fields";
import { ServiceCommandField } from "#/routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/services/$serviceId/-components/ServiceCommandField";
import type { ServiceDrawerState } from "#/routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/services/$serviceId/-components/useServiceDrawerState";

export function ServiceCommandsSection({
  state,
}: {
  state: ServiceDrawerState;
}) {
  const { service, collection, diff } = state;
  const startCommandDiff = diff.field(SERVICE_DEPLOYMENT_DIFF_PATHS.startCommand);
  const preDeployCommandDiff = diff.field(
    SERVICE_DEPLOYMENT_DIFF_PATHS.preDeployCommand,
  );

  return (
    <FieldGroup>
      <ServiceCommandField
        label="Start command"
        description="Run this command to start the service."
        addLabel="Start command"
        placeholder="npm start"
        value={service.startCommand}
        baselineLabel={startCommandDiff.baselineLabel}
        baselineValue={startCommandDiff.baselineValue}
        isChanged={startCommandDiff.changed}
        onCommit={(value) =>
          collection.update(service.id, (draft) => {
            draft.startCommand = value;
          })
        }
      />

      <ServiceCommandField
        label="Pre-deploy command"
        description="Run this command before each deploy."
        addLabel="Pre-deploy command"
        placeholder="npm run migrate"
        value={service.preDeployCommand}
        baselineLabel={preDeployCommandDiff.baselineLabel}
        baselineValue={preDeployCommandDiff.baselineValue}
        isChanged={preDeployCommandDiff.changed}
        addButtonVariant="link"
        addButtonSize="sm"
        collapsedAppearance="compact"
        onCommit={(value) =>
          collection.update(service.id, (draft) => {
            draft.preDeployCommand = value;
          })
        }
      />
    </FieldGroup>
  );
}
