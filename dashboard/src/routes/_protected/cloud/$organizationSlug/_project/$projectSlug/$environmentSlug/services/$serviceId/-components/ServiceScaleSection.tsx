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
import { serviceSetting } from "#/modules/config-store/catalog";
import type { StoreServiceRef } from "#/modules/config-store/store.contract";
import { environmentSettingsQuery, useStoreView } from "#/modules/config-store/store-view.queries";
import { useStoreWriter } from "#/modules/config-store/store-write";
import { ServiceSettingInput } from "#/routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/services/$serviceId/-components/ServiceSettingInput";
import type { ServiceDrawerState } from "#/routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/services/$serviceId/-components/useServiceDrawerState";

export function ServiceScaleSection({ state }: { state: ServiceDrawerState }) {
  return (
    <FieldGroup>
      {state.store
        ? <StoreReplicasField organizationSlug={state.organizationSlug} store={state.store} />
        : <ReplicasField state={state} />}
    </FieldGroup>
  );
}

/** Replicas in the Config Store: the catalog's words and bounds, the Store's value, saved through its edit queue. */
function StoreReplicasField({ organizationSlug, store }: { organizationSlug: string; store: StoreServiceRef }) {
  const setting = serviceSetting("replicas");
  const view = useStoreView(organizationSlug, environmentSettingsQuery(store.environment));
  const writer = useStoreWriter(organizationSlug);
  const path = `${store.service}.replicas`;
  const row = view.ok ? view.value.settings.find((candidate) => candidate.path === path) : undefined;

  return (
    <Field>
      <FieldLabel>{setting.title}</FieldLabel>
      <FieldDescription>{setting.description}</FieldDescription>
      {row ? (
        <ServiceSettingInput
          ariaLabel={setting.title}
          type="number"
          inputMode="numeric"
          min={setting.minimum}
          max={setting.maximum}
          step={1}
          suffix="replicas"
          value={String(row.value)}
          // TODO(#1270): the pink trail comes from the Store's diff view.
          isChanged={false}
          validate={(raw) => {
            const value = Number(raw);
            return raw.length > 0 && Number.isInteger(value) && value >= setting.minimum && value <= setting.maximum
              ? null
              : `Enter a whole number from ${setting.minimum} to ${setting.maximum}.`;
          }}
          onCommit={(raw) => writer.edit({
            environment: store.environment,
            changes: [{ op: "set", path, value: Number(raw) }],
          })}
        />
      ) : (
        <FieldDescription>{view.ok ? `The Config Store has no Service named ${store.service} here.` : view.refusal.message}</FieldDescription>
      )}
    </Field>
  );
}

function ReplicasField({ state }: { state: ServiceDrawerState }) {
  const { service, collection, diff } = state;
  const replicasDiff = diff.field(SERVICE_DEPLOYMENT_DIFF_PATHS.replicas);

  return (
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
  );
}
