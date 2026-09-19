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
  serviceMaxRetriesSchema,
  type ServiceRestartPolicy,
} from "#/modules/environment-design/services";
import { isValid } from "#/modules/environment-design/schema";
import { ServiceSettingInput } from "#/routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/services/$serviceId/-components/ServiceSettingInput";
import type { ServiceDrawerState } from "#/routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/services/$serviceId/-components/useServiceDrawerState";

const RESTART_POLICY_LABELS = {
  "unless-stopped": "Unless stopped",
  always: "Always",
  "on-failure": "On Failure",
  no: "Never",
} as const satisfies Record<ServiceRestartPolicy, string>;

const RESTART_POLICY_DESCRIPTIONS = {
  "unless-stopped": "Restart the container unless it was manually stopped.",
  always: "Restart the container whenever it stops.",
  "on-failure": "Restart the container if it exits with a non-zero exit code.",
  no: "Never restart the container if it stops.",
} as const satisfies Record<ServiceRestartPolicy, string>;

function RestartPolicyOption({ policy }: { policy: ServiceRestartPolicy }) {
  return (
    <span className="grid gap-1 whitespace-normal">
      <span>{RESTART_POLICY_LABELS[policy]}</span>
      <span className="text-muted-foreground">
        {RESTART_POLICY_DESCRIPTIONS[policy]}
      </span>
    </span>
  );
}

export function ServiceRestartPolicySection({
  state,
}: {
  state: ServiceDrawerState;
}) {
  const { service, collection, diff } = state;
  const restartDiff = diff.field(SERVICE_DEPLOYMENT_DIFF_PATHS.restartPolicy);
  const retriesDiff = diff.field(SERVICE_DEPLOYMENT_DIFF_PATHS.maxRetries);
  const cronDiff = diff.field(SERVICE_DEPLOYMENT_DIFF_PATHS.cron);

  return (
    <FieldGroup>
      <Field>
        <FieldLabel htmlFor="service-restart-policy">Restart policy</FieldLabel>
        <FieldDescription>
          Configure what to do when the process exits.
        </FieldDescription>
        <Select
          value={service.restartPolicy}
          onValueChange={(next) => {
            if (next !== "always" && next !== "on-failure" && next !== "no") return;
            const transaction = collection.update(service.id, (draft) => {
              draft.restartPolicy = next;
              if (next === "on-failure" && draft.maxRetries === 0) {
                draft.maxRetries = 10;
              }
            });
            void transaction.isPersisted.promise;
          }}
        >
          <SelectTrigger
            id="service-restart-policy"
            className="w-full data-[size=default]:h-auto"
            data-changed={restartDiff.changed || undefined}
          >
            <SelectValue>
              <RestartPolicyOption policy={service.restartPolicy} />
            </SelectValue>
          </SelectTrigger>
          <SelectContent>
            <SelectGroup>
              {(["always", "on-failure", "no"] as const).map((policy) => (
                <SelectItem key={policy} value={policy} label={RESTART_POLICY_LABELS[policy]}>
                  <RestartPolicyOption policy={policy} />
                </SelectItem>
              ))}
            </SelectGroup>
          </SelectContent>
        </Select>
      </Field>

      {service.restartPolicy === "on-failure" ? (
        <Field>
          <FieldDescription>
            Number of times to try and restart the service if it stopped due to an error.
          </FieldDescription>
          <ServiceSettingInput
            ariaLabel="Restart retries"
            type="number"
            inputMode="numeric"
            min={1}
            max={100}
            step={1}
            value={String(service.maxRetries)}
            isChanged={retriesDiff.changed}
            baselineLabel={retriesDiff.baselineLabel}
            baselineValue={retriesDiff.baselineValue}
            validate={(raw) =>
              Number(raw) >= 1 && isValid(serviceMaxRetriesSchema, Number(raw))
                ? null
                : "Enter a whole number from 1 to 100."
            }
            onCommit={(raw) =>
              collection.update(service.id, (draft) => {
                draft.maxRetries = Number(raw);
              })
            }
          />
        </Field>
      ) : null}

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
