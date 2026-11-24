import { sql } from "drizzle-orm";

import { timestamp } from "drizzle-orm/pg-core";



export type JsonPrimitive = string | number | boolean | null;

export type JsonValue = JsonPrimitive | JsonValue[] | JsonObject;

export type JsonObject = { [key: string]: JsonValue };

export type EncryptedSecretValue = {
  version: 1;
  iv: string;
  tag: string;
  ciphertext: string;
};

/**
 * The time a check compares stored times against: Postgres's clock as it checks each row. `clock_timestamp()` is
 * volatile, so when an update or a `for update` waits on a row's lock and Postgres rechecks the row after the wait, it
 * reads the clock again: a deadline that passed during the wait fails. `now()` is when the transaction began, and a
 * `(select clock_timestamp())` is read once before any wait, so both pass it (measured on Postgres 16). A volatile
 * value is no index key, so the time column narrows nothing; use {@link sweepCutoff} where that index matters.
 */
export function checkedNow() {
  return sql<Date>`clock_timestamp()`;
}

/**
 * The cutoff for a sweep that needs an index on the time column: `now()`, one value for the statement, so Postgres scans
 * the index range. It is when the transaction began and a lock-wait recheck reuses it, so a row that expires meanwhile
 * goes on the next run. Only for sweeps where acting on a just-expired row one run late is harmless; a check that
 * grants or refuses reads {@link checkedNow}.
 */
export function sweepCutoff() {
  return sql<Date>`now()`;
}

export function sqlStringLiterals(values: readonly string[]) {
  return sql.raw(
    values.map((value) => `'${value.replaceAll("'", "''")}'`).join(","),
  );
}

export const createdAt = timestamp("created_at", {
  mode: "date",
  withTimezone: true,
})
  .defaultNow()
  .notNull();

export const updatedAt = timestamp("updated_at", {
  mode: "date",
  withTimezone: true,
})
  .defaultNow()
  .notNull();
