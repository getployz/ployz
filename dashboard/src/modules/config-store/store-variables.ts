import { Schema } from "effect";
import type { Change, EnvironmentRef, EnvironmentView, RowId, SecretValue, ServiceListing, SettingRow } from "@ployz/sdk";
import { getManagedServiceExports } from "#/modules/variables/managed-service-exports";
import { buildReferenceTargets } from "#/modules/variables/variable-autocomplete";
import { renameReferences } from "#/modules/variables/variable-template";
import type { VariableWriter } from "#/modules/variables/variables";
import type { VariableRecord } from "#/modules/variables/variables";
import type { SealItem, Sealing } from "#/modules/variables/sealing";
import { SealError } from "#/modules/sealing/seal-error";
import { serviceSettingRows } from "./store-services";
import type { useStoreWriter } from "./store-write";

const VARIABLE = /^env\.([^.]+)$/u;
const SERVICE_VARIABLE = /^([^.]+)\.env\.([^.]+)$/u;
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
  /** What its Environment marks Never sync (`EnvironmentView.never_synced`). */
  neverSynced: ReadonlySet<RowId> = new Set(),
  /** Why each variable with a broken reference is broken, by key (`brokenVariables`). */
  broken: ReadonlyMap<string, string> = new Map()): VariableRecord[] {
  return [...rows].flatMap(([name, row]) => {
    const key = VARIABLE.exec(name)?.[1];
    if (key === undefined || row.value === null) return [];
    const { value } = row;
    return [{
      id: key, serviceId, key, description: null,
      exported: rows.get(`${name}.exported`)?.value === true,
      value: isText(value) ? { type: "plain" as const, value } : { type: "sealed" as const, notSet: isValueless(value) },
      changed: changes.has(name) || changes.has(`${name}.exported`),
      neverSynced: row.row !== undefined && neverSynced.has(row.row),
      broken: broken.get(key),
    }];
  }).sort((a, b) => a.key.localeCompare(b.key));
}

/** What a Sealed value shows while it saves: a secret with a value, never the value. It never reaches the Store. */
const SEALING = { secret: { sealing: true } };

/** A change, or a Sealed value of variable `key` that `useSealing`'s `prepare` turns into one in the edit's turn. */
type Write = Change | { readonly key: string; readonly sealed: string };

const UNPLACED = "Set this value after merging.";

const leavesValueless = Schema.is(Schema.Struct({ after: Schema.Struct({ secret: Schema.Literal(false) }) }));

/**
 * The keys of a Service's secrets without a value that no staged value change sets: deploys skip them until they
 * are set. Staging only `.exported` sets nothing, and neither does History bringing one back Not set.
 */
export function notSetVariables(rows: ReadonlyMap<string, SettingRow>, changes: ReadonlyMap<string, unknown>) {
  return serviceVariables(rows, "", changes).flatMap((variable) => {
    const change = changes.get(`env.${variable.key}`);
    return variable.value.type === "sealed" && variable.value.notSet && (change === undefined || leavesValueless(change))
      ? [variable.key] : [];
  });
}

/**
 * The variables panel's writer over the Store: each write is one optimistic edit of `SERVICE.env.KEY` (and its
 * `.exported`), the one place those paths are spelled.
 */
export function storeVariableWriter(
  writer: ReturnType<typeof useStoreWriter>, environment: EnvironmentRef, service: string, variables: readonly VariableRecord[],
  /** The Service's Setting rows (`serviceSettingRows`): each variable's row, which Never sync names. */
  settings: ReadonlyMap<string, SettingRow>,
  /** How a Sealed value is sent, and the ids it is sealed for: this Environment's and this Service's own. */
  sealing: { readonly prepare: Sealing["prepare"]; readonly target: SealItem["target"] },
) {
  const path = (key: string) => `${service}.env.${key}`;
  const edit = (...changes: Change[]) => writer.edit({ environment, changes });
  const secret = (key: string, value: SecretValue | typeof SEALING.secret): Change => ({ op: "set", path: path(key), value: { secret: value } });
  /** One edit; its Sealed values show as `SEALING` and are prepared together, all or none. */
  const write = (writes: readonly Write[]) => {
    const sealed = writes.flatMap((one) => "op" in one ? [] : [{ key: one.key, value: one.sealed, target: sealing.target }]);
    if (sealed.length === 0) return edit(...writes.flatMap((one) => "op" in one ? [one] : []));
    return writer.edit({
      environment,
      changes: writes.map((one) => "op" in one ? one : secret(one.key, SEALING.secret)),
      prepare: async () => {
        const values = (await sealing.prepare(sealed)).values();
        return writes.map((one) => {
          if ("op" in one) return one;
          const { done, value } = values.next();
          if (done) throw new Error("Sealed values went unprepared.");
          return secret(one.key, value);
        });
      },
    });
  };
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
    create: (key: string, value: string, sealed: boolean, exported: boolean) => write([
      sealed ? { key, sealed: value } : { op: "set", path: path(key), value },
      { op: "set", path: `${path(key)}.exported`, value: exported },
    ]),
    /**
     * Renames it, value, seal and export alike; the Store refuses a name in use. With `updateReferences`, the variables
     * that reference it switch to the new name; without, the Store refuses a referenced variable.
     */
    rename: (key: string, to: string, updateReferences = false) => {
      const change: Change = { op: "rename", path: path(key), to };
      // An older Store refuses the field, so it goes only with references to update.
      if (updateReferences) change.update_references = true;
      return edit(change);
    },
    /** Seals a plain variable's value, or sets a Not set one's. */
    seal: (key: string, value: string) => write([{ key, sealed: value }]),
    export: (key: string, exported: boolean) => edit({ op: "set", path: `${path(key)}.exported`, value: exported }),
    /** Marks the variable Never sync, or with `marked` false syncs it again: at once, nothing to deploy. */
    neverSync: (key: string, marked: boolean) => {
      const row = settings.get(`env.${key}`)?.row;
      if (!row) throw new Error("Variable is not loaded.");
      return writer.commit({ command: "never_sync", environment, rows: [row], off: !marked });
    },
    /** The raw editor's changes: sets, the new ones it marks Sealed sealed, and removals in one edit. */
    replace: (sets: readonly { key: string; value: string; sealed?: boolean }[], removed: readonly string[]) => write([
      ...sets.map(({ key, value, sealed }): Write => sealed ? { key, sealed: value } : { op: "set", path: path(key), value }),
      ...removed.map((key): Change => ({ op: "unset", path: path(key) })),
    ]),
  };
}

/** The variables of `view` that reference variable `key` of Service `service`, as `SERVICE.KEY`: what a rename switches. */
export function variableReferrers(view: EnvironmentView, service: string, key: string): string[] {
  return view.settings.flatMap(({ path, value }) => {
    const [, owner, own] = SERVICE_VARIABLE.exec(path) ?? [];
    if (owner === undefined || own === undefined || !isText(value)) return [];
    return renameReferences(value, owner, service, key, key) === null ? [] : [`${owner}.${own}`];
  });
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

/** A Sync or On merge row's node lineage, and its variable key when it is one, as the Store reads the RowId. */
function rowAt(row: RowId) {
  const colon = row.indexOf(":");
  if (colon <= 0) return null;
  const at = row.slice(colon + 1);
  return { lineage: row.slice(0, colon), key: at.startsWith("variables.") && at.length > "variables.".length ? at.slice("variables.".length) : null };
}

/**
 * The Service of `services` (a receiving Environment's) that a Sync or On merge row lands in: the one of its node's
 * lineage, matched by `ServiceListing.row` and never by name. None while it is new there or being removed.
 */
export function receivingService(row: RowId, services: readonly ServiceListing[]) {
  const lineage = rowAt(row)?.lineage;
  return lineage === undefined ? undefined : services.find((service) => service.row === `${lineage}:node` && service.change !== "delete");
}

/**
 * What a Sync or On merge sends for secret rows given values typed for them: each value sealed for the Service it lands
 * in, to the key a fresh read returns. A row whose Service is new in the receiver
 * has nothing to seal for: with `unplaced` "drop" its value stays out and the row arrives Not set, with "refuse" the
 * write is refused, since leaving it out would take back an On merge Edit.
 */
export async function receivingValues<T extends { readonly row: RowId; readonly value: string }>(
  sealing: Pick<Sealing, "sealer">, typed: readonly T[], receiver: () => Promise<{ environment: string; services: readonly ServiceListing[] }>,
  unplaced: "drop" | "refuse",
): Promise<(T & { value: SecretValue })[]> {
  const seal = await sealing.sealer();
  const { environment, services } = await receiver();
  return (await Promise.all(typed.map(async (one) => {
    const key = rowAt(one.row)?.key;
    if (!key) throw new Error(`${one.row} is not a variable.`);
    const service = receivingService(one.row, services);
    if (!service && unplaced === "refuse") throw new SealError(UNPLACED);
    if (!service) return [];
    return [{ ...one, value: await seal({ key, value: one.value, target: { environment, service: service.id } }) }];
  }))).flat();
}
