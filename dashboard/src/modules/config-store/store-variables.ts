import { Schema } from "effect";
import type { Change, EnvironmentRef, EnvironmentView, ServiceListing, SettingRow } from "@ployz/sdk";
import { getManagedServiceExports } from "#/modules/variables/managed-service-exports";
import { buildReferenceTargets } from "#/modules/variables/variable-autocomplete";
import type { VariableWriter } from "#/modules/variables/variables";
import type { VariableRecord } from "#/modules/variables/variables";
import { serviceSettingRows } from "./store-services";
import type { useStoreWriter } from "./store-write";

// The variables panel shows `VariableRecord`s; the Store keys a variable by its name and has no timestamps.
const NEVER = new Date(0);
const VARIABLE = /^env\.([^.]+)$/u;
const isText = Schema.is(Schema.String);

/**
 * One Service's variables from its Setting rows (`serviceSettingRows`), sorted by key. A secret reads as
 * `{"secret": true}` and shows sealed; an unset one (null, pending removal) is gone.
 */
export function serviceVariables(rows: ReadonlyMap<string, SettingRow>, serviceId: string): VariableRecord[] {
  return [...rows].flatMap(([name, row]) => {
    const key = VARIABLE.exec(name)?.[1];
    if (key === undefined || row.value === null) return [];
    const { value } = row;
    return [{
      id: key, serviceId, key, description: null,
      exported: rows.get(`${name}.exported`)?.value === true,
      value: isText(value) ? { type: "plain" as const, value } : { type: "sealed" as const, hasValue: true as const, fingerprint: "sealed" },
      createdAt: NEVER, updatedAt: NEVER,
    }];
  }).sort((a, b) => a.key.localeCompare(b.key));
}

/** The variables panel's writer over the Store: each write is one optimistic edit of `SERVICE.env.KEY`. */
export function storeVariableWriter(
  writer: ReturnType<typeof useStoreWriter>, environment: EnvironmentRef, service: string, variables: readonly VariableRecord[],
): VariableWriter {
  const path = (key: string) => `${service}.env.${key}`;
  const edit = (...changes: Change[]) => writer.edit({ environment, changes });
  // A variable is two rows, its value and whether it's exported; the dashboard sets each by its own path.
  const rows = (variable: VariableRecord): Change[] => [
    { op: "set", path: path(variable.key), value: variable.value.type === "plain" ? variable.value.value : { secret: true } },
    { op: "set", path: `${path(variable.key)}.exported`, value: variable.exported },
  ];
  return {
    insert: (variable) => edit(...rows(variable)),
    update(key, updater) {
      const variable = structuredClone(variables.find((candidate) => candidate.key === key));
      if (!variable) throw new Error("Variable is not loaded.");
      updater(variable);
      return edit(...rows(variable));
    },
    delete: (key) => edit({ op: "unset", path: path(key) }),
  };
}

/** The variables Ployz adds to a Service. Its domains come with the Store's networking Settings. */
export function storeManagedExports(service: ServiceListing, environment: EnvironmentView["environment"]) {
  return getManagedServiceExports({
    id: service.id, lineageId: service.id, name: service.name, slug: service.name, privateDns: service.private_dns,
    environmentId: environment.id, environmentSlug: environment.name,
    // ponytail: no domains until the Store has networking (#1265), so no PLOYZ_PUBLIC_DOMAIN.
    routes: [], managedHostnames: [],
  });
}

/** What a value's `${{ }}` autocomplete offers: this Service's variables and every other Service's exported ones. */
export function storeReferenceTargets(view: EnvironmentView, services: readonly ServiceListing[], self: string) {
  return buildReferenceTargets({
    services: services.map((service) => ({
      slug: service.name, name: service.name, isSelf: service.id === self,
      variables: serviceVariables(serviceSettingRows(view, service.name), service.id)
        .map((variable) => ({ key: variable.key, exported: variable.exported, isSecret: variable.value.type === "sealed", description: null })),
      managedExports: storeManagedExports(service, view.environment),
    })),
  });
}
