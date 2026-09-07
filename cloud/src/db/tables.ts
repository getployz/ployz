import { sql } from "drizzle-orm";

import { timestamp } from "drizzle-orm/pg-core";



export type JsonPrimitive = string | number | boolean | null;

export type JsonValue = JsonPrimitive | JsonValue[] | JsonObject;

export type JsonObject = { [key: string]: JsonValue };

export type MachineId = string;

export type EncryptedSecretValue = {
  version: 1;
  iv: string;
  tag: string;
  ciphertext: string;
};

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
