import {
  Field,
  FieldDescription,
  FieldGroup,
  FieldLabel,
} from "#/components/ui/field";
import { SERVICE_DEPLOYMENT_DIFF_PATHS } from "#/modules/services/service-deployment-diff/fields";
import {
  serviceReplicasSchema,
} from "#/modules/environment-design/services";
import { isValid } from "#/modules/environment-design/schema";
import { ServiceSettingInput } from "#/routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/services/$serviceId/-components/ServiceSettingInput";
import type { ServiceDrawerState } from "#/routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/services/$serviceId/-components/useServiceDrawerState";

export function ServiceScaleSection({ state }: { state: ServiceDrawerState }) {
  const { service, collection, diff } = state;
  const replicasDiff = diff.field(SERVICE_DEPLOYMENT_DIFF_PATHS.replicas);

  return (
    <FieldGroup>
      <Field>
        <FieldLabel>Replicas</FieldLabel>
        <FieldDescription>Running instances.</FieldDescription>
        <ServiceSettingInput
          ariaLabel="Replicas"
          type="number"
          inputMode="numeric"
          min={0}
          max={50}
          step={1}
          suffix="replicas"
          value={String(service.replicas)}
          isChanged={replicasDiff.changed}
          validate={(raw) =>
            raw.length > 0 &&
            isValid(serviceReplicasSchema, Number(raw))
              ? null
              : "Enter a whole number from 0 to 50."
          }
          onCommit={(raw) =>
            collection.update(service.id, (draft) => {
              draft.replicas = Number(raw);
            })
          }
        />
      </Field>

    </FieldGroup>
  );
}
