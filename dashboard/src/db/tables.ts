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
 * The time a check compares stored times against: Postgres's clock as the statement reads it, so a transaction that
 * waited on a lock sees the time after the wait, not `now()`, which is when the transaction began. The scalar subquery
 * runs once per statement, so an index still serves the comparison, which a bare volatile `clock_timestamp()` defeats.
 * An update that waits on its row's lock rechecks the row against the time after that wait too (measured on Postgres 16).
 */
export function checkedNow() {
  return sql<Date>`(select clock_timestamp())`;
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
