import {
  Field,
  FieldDescription,
  FieldError,
  FieldGroup,
  FieldLabel,
} from "#/components/ui/field";
import {
  Select,
  SelectContent,
  SelectGroup,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "#/components/ui/select";
import { SERVICE_DEPLOYMENT_DIFF_PATHS } from "#/modules/services/service-deployment-diff/fields";
import {
  SERVICE_RESTART_POLICIES,
  type ServiceRestartPolicy,
} from "#/modules/environment-design/services";
import { ServiceSettingInput } from "#/routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/services/$serviceId/-components/ServiceSettingInput";
import type { ServiceDrawerState } from "#/routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/services/$serviceId/-components/useServiceDrawerState";

const RESTART_POLICY_LABELS = {
  "unless-stopped": "Unless stopped",
  always: "Always",
  "on-failure": "On failure",
  no: "No",
} as const satisfies Record<ServiceRestartPolicy, string>;

export function ServiceRestartPolicySection({
  state,
}: {
  state: ServiceDrawerState;
}) {
  const { service, collection, diff } = state;
  const restartDiff = diff.field(SERVICE_DEPLOYMENT_DIFF_PATHS.restartPolicy);
  const cronDiff = diff.field(SERVICE_DEPLOYMENT_DIFF_PATHS.cron);
  // "on-failure" (with bounded max-retries) and cron schedules aren't yet
  // deployable by the Rust runtime. Block newly selecting/configuring them,
  // but keep existing values visible so users can switch away.
  const restartPolicyUnsupported = service.restartPolicy === "on-failure";

  return (
    <FieldGroup>
      <Field data-invalid={restartPolicyUnsupported || undefined}>
        <FieldLabel htmlFor="service-restart-policy">Restart policy</FieldLabel>
        <FieldDescription>
          What to do when the container exits.
        </FieldDescription>
        <Select
          value={service.restartPolicy}
          onValueChange={(next) => {
            const transaction = collection.update(service.id, (draft) => {
              // SAFETY: Select only emits SERVICE_RESTART_POLICIES values from the items below.
              draft.restartPolicy = next as ServiceRestartPolicy;
            });
            void transaction.isPersisted.promise;
          }}
        >
          <SelectTrigger
            id="service-restart-policy"
            aria-invalid={restartPolicyUnsupported || undefined}
            className="w-full"
            data-changed={restartDiff.changed || undefined}
          >
            <SelectValue />
          </SelectTrigger>
          <SelectContent>
            <SelectGroup>
              {SERVICE_RESTART_POLICIES.map((policy) => (
                <SelectItem
                  key={policy}
                  value={policy}
                  disabled={policy === "on-failure" && !restartPolicyUnsupported}
                >
                  {RESTART_POLICY_LABELS[policy]}
                </SelectItem>
              ))}
            </SelectGroup>
          </SelectContent>
        </Select>
        {restartPolicyUnsupported ? (
          <FieldError>
            Select Always, Unless stopped, or No before deploying
          </FieldError>
        ) : null}
      </Field>

      {service.cron ? (
        <Field data-invalid>
          <FieldLabel>Cron schedule</FieldLabel>
          <FieldDescription>
            The service is configured to run on this schedule
          </FieldDescription>
          <ServiceSettingInput
            ariaLabel="Cron schedule"
            placeholder="0 3 * * *"
            value={service.cron}
            isChanged={cronDiff.changed}
            baselineLabel={cronDiff.baselineLabel}
            baselineValue={cronDiff.baselineValue}
            validate={(raw) =>
              raw.length === 0 ? null : "Clear this schedule before deploying"
            }
            onCommit={(raw) =>
              collection.update(service.id, (draft) => {
                draft.cron = raw.length > 0 ? raw : null;
              })
            }
          />
          <FieldError>Clear this schedule before deploying</FieldError>
        </Field>
      ) : null}
    </FieldGroup>
  );
}
