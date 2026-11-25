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
 * The time a check compares stored times against: `clock_timestamp()`, Postgres's clock each time it is read.
 * `now()` is when the transaction began. What a statement that waits on a row lock sees, measured on Postgres 16.16:
 * - An UPDATE or DELETE rechecks its WHERE after the wait, reading the clock again, only when the holder changed the
 *   row and committed. Then a deadline that passed during the wait refuses the row.
 * - When the holder only locked the row (`for update`, then commit or rollback), the row is unchanged, Postgres skips
 *   the recheck, and the UPDATE or DELETE keeps the check it made before the wait: an expired row still passes.
 * - A `select … for update` or `for share` evaluates its WHERE and columns before it waits, so its check is from before.
 * In a transaction, lock the row by id in one statement and check against this in the next: that check follows every wait.
 * A volatile value is no index key, so the time column narrows nothing; a plain range read or prune that needs that
 * index reads {@link sweepCutoff}.
 */
export function checkedNow() {
  return sql<Date>`clock_timestamp()`;
}

/**
 * The cutoff for a plain range read or a prune that needs an index on the time column: `now()`, one value for the
 * statement, so Postgres scans the index range. It is when the transaction began, so a row that expires meanwhile goes
 * on the next run. A statement that locks, updates, or returns the rows it deletes checks against {@link checkedNow}.
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
