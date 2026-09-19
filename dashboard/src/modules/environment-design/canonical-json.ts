import { Schema } from "effect";

type JsonRecord = { [key: string]: typeof Schema.Json.Type };
function isJsonRecord(value: typeof Schema.Json.Type): value is JsonRecord {
  return value !== null && typeof value === "object" && !Array.isArray(value);
}

/** Stable JSON for authored publication equality and review fingerprints. */
export function canonicalJson<Input>(input: Input, omittedKeys: readonly string[] = []): string {
  const value = Schema.decodeUnknownSync(Schema.Json)(JSON.parse(JSON.stringify(input)));
  return JSON.stringify(value, (_key, entry: typeof Schema.Json.Type) => isJsonRecord(entry)
    ? Object.fromEntries(Object.entries(entry)
      .filter(([key]) => !omittedKeys.includes(key))
      .sort(([a], [b]) => a < b ? -1 : a > b ? 1 : 0))
    : entry);
}
