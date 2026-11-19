import type { JsonValue } from "@ployz/sdk";
import { asRecord } from "#/lib/json";

/** One line of a Service's replicas: how many run in a region. No region is any region, the Environment having none. */
export type ReplicaRow = { readonly region: string | null; readonly count: number };

/** The most replicas a Service runs in all, whatever the regions: the Engine's own limit. */
export const REPLICAS_MAX = 50;

const isCount = (value: JsonValue): value is number => typeof value === "number" && Number.isInteger(value) && value >= 0;

/** A `replicas` value that names its regions, as a row per region in key order. Null for a bare count or one unreadable. */
function namedRows(value: JsonValue): ReplicaRow[] | null {
  const map = Array.isArray(value) ? null : asRecord(value);
  if (map === null) return null;
  const rows = Object.keys(map).sort().flatMap((region) => {
    const count = map[region];
    return count !== undefined && isCount(count) ? [{ region, count }] : [];
  });
  return rows.length > 0 ? rows : null;
}

/**
 * The rows a `replicas` value reads as. A bare count runs in the Environment's region (`home`), or anywhere with none.
 * A map has a row per region. Unreadable reads as one replica.
 */
export function replicaRows(value: JsonValue, home: string | null): ReplicaRow[] {
  if (isCount(value)) return [{ region: home, count: value }];
  return namedRows(value) ?? [{ region: home, count: 1 }];
}

const sameRows = (a: readonly ReplicaRow[], b: readonly ReplicaRow[]) =>
  a.length === b.length && a.every((row, at) => row.region === b[at]?.region && row.count === b[at]?.count);

/**
 * The `replicas` value that says `rows`, from what is true now: `current` as the next Deploy has it, `deployed` as the
 * Service last ran with it. Rows that say what was deployed write the deployed value as it was, which stages nothing.
 * Otherwise a value that names its regions keeps naming them, so a later change of the Environment's region doesn't move
 * it. A bare count stays bare while its one row is in the Environment's region. Any region is a bare count, which no map can say.
 */
export function replicaValue(rows: readonly ReplicaRow[], home: string | null, current: JsonValue, deployed: JsonValue): JsonValue {
  if ((isCount(deployed) || namedRows(deployed) !== null) && sameRows(rows, replicaRows(deployed, home))) return deployed;
  const [only] = rows;
  if (rows.length === 1 && only !== undefined && (only.region === null || (only.region === home && namedRows(current) === null))) return only.count;
  return Object.fromEntries(rows.flatMap(({ region, count }) => (region === null ? [] : [[region, count]])));
}

/** `value` lowered to one replica, in the region it names so it doesn't come to follow the Environment's. Null when it names other than one. */
export function oneReplica(value: JsonValue): JsonValue {
  const regions = namedRows(value)?.flatMap(({ region, count }) => (region !== null && count > 0 ? [region] : []));
  if (regions === undefined) return 1;
  const [only, ...others] = regions;
  return only !== undefined && others.length === 0 ? { [only]: 1 } : null;
}

/** How many replicas a Service's `replicas` asks for across its regions. Null when it doesn't say. */
export function desiredReplicas(value: JsonValue | undefined): number | null {
  if (value === undefined) return null;
  if (isCount(value)) return value;
  return namedRows(value)?.reduce((sum, row) => sum + row.count, 0) ?? null;
}

/** The value as Details words it: `2`, or `fsn 2 · syd 1`. A bare count names the Environment's region when it has one. */
function replicaText(value: JsonValue, home: string | null): string {
  if (value === null) return "";
  const rows = replicaRows(value, home);
  const [only] = rows;
  if (rows.length === 1 && only !== undefined && only.region === null) return String(only.count);
  return rows.map(({ region, count }) => `${region ?? "any region"} ${count}`).join(" · ");
}

/**
 * A change of `replicas` as Details and Versions word it, each side in the Environment's region as it was then
 * (`homes`): `fsn 2 → fsn 2 · syd 1`. Where a bare count came to name the region it already ran in, both sides would
 * read the same, so the count stays bare: `2 → fsn 2`.
 */
export function replicaChange(before: JsonValue, after: JsonValue, homes: readonly [string | null, string | null]): [string, string] {
  const [was, now] = [replicaText(before, homes[0]), replicaText(after, homes[1])];
  if (was !== now || before === null || after === null) return [was, now];
  return [isCount(before) ? String(before) : was, isCount(after) ? String(after) : now];
}
