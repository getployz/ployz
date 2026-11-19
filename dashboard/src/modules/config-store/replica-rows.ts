import type { JsonValue } from "@ployz/sdk";
import { asRecord } from "#/lib/json";

/** One line of a Service's replicas: how many run in a region. No region is any region, the Environment having none. */
export type ReplicaRow = { readonly region: string | null; readonly count: number };

/** The most replicas a Service runs in all, whatever the regions: the Engine's own limit. */
export const REPLICAS_MAX = 50;

const isCount = (value: JsonValue): value is number => typeof value === "number" && Number.isInteger(value) && value >= 0;

/**
 * The rows a `replicas` value reads as. A bare count runs in the Environment's region (`home`), or anywhere with none.
 * A map has a row per region, in key order. Unreadable reads as one replica.
 */
export function replicaRows(value: JsonValue, home: string | null): ReplicaRow[] {
  if (isCount(value)) return [{ region: home, count: value }];
  const map = Array.isArray(value) ? null : asRecord(value);
  if (map !== null) {
    const rows = Object.keys(map).sort().flatMap((region) => {
      const count = map[region];
      return count !== undefined && isCount(count) ? [{ region, count }] : [];
    });
    if (rows.length > 0) return rows;
  }
  return [{ region: home, count: 1 }];
}

/**
 * The `replicas` value that says `rows`. One row in the Environment's region is the bare count, so the Service keeps
 * following the Environment where it moves. Any other rows are a map, which names its regions whatever the Environment is.
 */
export function replicaValue(rows: readonly ReplicaRow[], home: string | null): JsonValue {
  const [only] = rows;
  if (rows.length === 1 && only !== undefined && only.region === home) return only.count;
  return Object.fromEntries(rows.flatMap(({ region, count }) => (region === null ? [] : [[region, count]])));
}

/** How many replicas a value runs in all. */
export const replicaTotal = (value: JsonValue, home: string | null = null) => replicaRows(value, home).reduce((sum, row) => sum + row.count, 0);

/** The value as Details words it: `2`, or `fsn 2 · syd 1`. A bare count names the Environment's region when it has one. */
export function replicaText(value: JsonValue, home: string | null): string {
  const rows = replicaRows(value, home);
  const [only] = rows;
  if (rows.length === 1 && only !== undefined && only.region === null) return String(only.count);
  return rows.map(({ region, count }) => `${region ?? "any region"} ${count}`).join(" · ");
}
