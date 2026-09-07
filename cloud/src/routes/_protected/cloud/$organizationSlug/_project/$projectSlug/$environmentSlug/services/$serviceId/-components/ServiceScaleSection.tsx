import {
  Field,
  FieldDescription,
  FieldGroup,
  FieldLabel,
} from "#/components/ui/field";
import { SERVICE_DEPLOYMENT_DIFF_PATHS } from "#/modules/services/service-deployment-diff/fields";
import {
  serviceCpuLimitSchema,
  serviceMemLimitSchema,
  serviceReplicasSchema,
} from "#/modules/environment-design/services";
import { isValid } from "#/modules/environment-design/schema";
import { ServiceSettingInput } from "#/routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/services/$serviceId/-components/ServiceSettingInput";
import type { ServiceDrawerState } from "#/routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/services/$serviceId/-components/useServiceDrawerState";

export function ServiceScaleSection({ state }: { state: ServiceDrawerState }) {
  const { service, collection, diff } = state;
  const replicasDiff = diff.field(SERVICE_DEPLOYMENT_DIFF_PATHS.replicas);
  const cpuDiff = diff.field(SERVICE_DEPLOYMENT_DIFF_PATHS.cpuLimit);
  const memDiff = diff.field(SERVICE_DEPLOYMENT_DIFF_PATHS.memLimit);

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

      <Field>
        <FieldLabel>CPU</FieldLabel>
        <FieldDescription>vCPU per replica.</FieldDescription>
        <ServiceSettingInput
          ariaLabel="CPU limit"
          type="number"
          inputMode="decimal"
          max={64}
          step="any"
          suffix="vCPU"
          placeholder="Unlimited"
          value={service.cpuLimit == null ? "" : String(service.cpuLimit)}
          isChanged={cpuDiff.changed}
          validate={(raw) =>
            raw.length === 0 ||
            isValid(serviceCpuLimitSchema, Number(raw))
              ? null
              : "Enter a value above 0 and up to 64."
          }
          onCommit={(raw) =>
            collection.update(service.id, (draft) => {
              draft.cpuLimit = raw.length === 0 ? null : Number(raw);
            })
          }
        />
      </Field>

      <Field>
        <FieldLabel>Memory</FieldLabel>
        <FieldDescription>GB per replica.</FieldDescription>
        <ServiceSettingInput
          ariaLabel="Memory limit"
          type="number"
          inputMode="decimal"
          max={1024}
          step="any"
          suffix="GB"
          placeholder="Unlimited"
          value={service.memLimit == null ? "" : String(service.memLimit)}
          isChanged={memDiff.changed}
          validate={(raw) =>
            raw.length === 0 ||
            isValid(serviceMemLimitSchema, Number(raw))
              ? null
              : "Enter a value above 0 and up to 1024."
          }
          onCommit={(raw) =>
            collection.update(service.id, (draft) => {
              draft.memLimit = raw.length === 0 ? null : Number(raw);
            })
          }
        />
      </Field>
    </FieldGroup>
  );
}
