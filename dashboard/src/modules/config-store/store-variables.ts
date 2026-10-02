import { Schema } from "effect";
import type { Change, EnvironmentRef, EnvironmentView, ServiceListing, SettingRow } from "@ployz/sdk";
import { getManagedServiceExports } from "#/modules/variables/managed-service-exports";
import { buildReferenceTargets } from "#/modules/variables/variable-autocomplete";
import type { VariableWriter } from "#/modules/variables/variables";
import type { VariableRecord } from "#/modules/variables/variables";
import { serviceSettingRows } from "./store-services";
import type { useStoreWriter } from "./store-write";

const VARIABLE = /^env\.([^.]+)$/u;
const isText = Schema.is(Schema.String);
const isValueless = Schema.is(Schema.Struct({ secret: Schema.Literal(false) }));

/**
 * One Service's variables from its Setting rows (`serviceSettingRows`), sorted by key. A secret reads as
 * `{"secret": true}`, or `{"secret": false}` until it has a value, and shows sealed; an unset one (null, pending
 * removal) is gone.
 */
export function serviceVariables(rows: ReadonlyMap<string, SettingRow>, serviceId: string,
  /** What the next Deploy changes, by Setting (`env.KEY`): those rows are pink. */
  changes: ReadonlyMap<string, unknown> = new Map(),
  /** What its Environment marks Never sync, by Setting (`env.KEY`). */
  neverSynced: ReadonlySet<string> = new Set()): VariableRecord[] {
  return [...rows].flatMap(([name, row]) => {
    const key = VARIABLE.exec(name)?.[1];
    if (key === undefined || row.value === null) return [];
    const { value } = row;
    return [{
      id: key, serviceId, key, description: null,
      exported: rows.get(`${name}.exported`)?.value === true,
      value: isText(value) ? { type: "plain" as const, value } : { type: "sealed" as const, needsValue: isValueless(value) },
      changed: changes.has(name) || changes.has(`${name}.exported`),
      neverSynced: neverSynced.has(name),
    }];
  }).sort((a, b) => a.key.localeCompare(b.key));
}

/** What the Environment marks Never sync in one Service, by Setting (`env.KEY`), as `serviceVariables` takes it. */
export function serviceNeverSynced(view: EnvironmentView, service: string): Set<string> {
  return new Set((view.never_synced ?? []).flatMap((row) => row.node === service && row.name !== null ? [row.name] : []));
}

/**
 * The variables panel's writer over the Store: each write is one optimistic edit of `SERVICE.env.KEY` (and its
 * `.exported`), the one place those paths are spelled.
 */
export function storeVariableWriter(
  writer: ReturnType<typeof useStoreWriter>, environment: EnvironmentRef, service: string, variables: readonly VariableRecord[],
  /** The Service's Setting rows (`serviceSettingRows`): each variable's row, which Never sync names. */
  settings: ReadonlyMap<string, SettingRow>,
) {
  const path = (key: string) => `${service}.env.${key}`;
  const edit = (...changes: Change[]) => writer.edit({ environment, changes });
  // A variable is two rows, its value and whether it's exported; the dashboard sets each by its own path.
  const rows = (variable: VariableRecord): Change[] => [
    { op: "set", path: path(variable.key), value: variable.value.type === "plain" ? variable.value.value : { secret: true } },
    { op: "set", path: `${path(variable.key)}.exported`, value: variable.exported },
  ];
  const panel: VariableWriter = {
    insert: (variable) => edit(...rows(variable)),
    update(key, updater) {
      const variable = structuredClone(variables.find((candidate) => candidate.key === key));
      if (!variable) throw new Error("Variable is not loaded.");
      updater(variable);
      return edit(...rows(variable));
    },
    delete: (key) => edit({ op: "unset", path: path(key) }),
  };
  return {
    ...panel,
    create: (key: string, value: string, sealed: boolean, exported: boolean) => edit(
      { op: "set", path: path(key), value: sealed ? { secret: value } : value },
      { op: "set", path: `${path(key)}.exported`, value: exported },
    ),
    seal: (key: string, value: string) => edit({ op: "set", path: path(key), value: { secret: value } }),
    export: (key: string, exported: boolean) => edit({ op: "set", path: `${path(key)}.exported`, value: exported }),
    /** Marks the variable Never sync, or with `marked` false syncs it again: at once, nothing to deploy. */
    neverSync: (key: string, marked: boolean) => {
      const row = settings.get(`env.${key}`)?.row;
      if (!row) throw new Error("Variable is not loaded.");
      return writer.commit({ command: "never_sync", environment, rows: [row], off: !marked });
    },
    /** The raw editor's changes: sets and removals in one edit. */
    replace: (sets: readonly { key: string; value: string }[], removed: readonly string[]) => edit(
      ...sets.map(({ key, value }): Change => ({ op: "set", path: path(key), value })),
      ...removed.map((key): Change => ({ op: "unset", path: path(key) })),
    ),
  };
}

/** Default built-in reference values; authored variables take precedence. */
export function storeManagedExports(service: ServiceListing, environment: EnvironmentView["environment"]) {
  return getManagedServiceExports({
    id: service.id, lineageId: service.id, name: service.name, slug: service.name, privateDns: service.private_dns,
    environmentId: environment.id, environmentSlug: environment.name,
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
