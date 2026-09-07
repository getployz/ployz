import { Option, Schema } from "effect";
import type { JsonObject, JsonValue } from "#/db/tables";

const RecordValue = Schema.Record(Schema.String, Schema.Unknown);
const decodeRecord = Schema.decodeUnknownOption(RecordValue);
const decodeString = Schema.decodeUnknownOption(Schema.String);
const decodeFinite = Schema.decodeUnknownOption(Schema.Finite);
const decodeBoolean = Schema.decodeUnknownOption(Schema.Boolean);
const decodeBigInt = Schema.decodeUnknownOption(Schema.BigInt);
const JsonPrimitive = Schema.Union([
  Schema.String,
  Schema.Finite,
  Schema.Boolean,
  Schema.Null,
]);
const decodeJsonPrimitive = Schema.decodeUnknownOption(JsonPrimitive);

export function asRecord<T>(value: T): JsonObject | null {
  const decoded = decodeRecord(value);
  if (Option.isNone(decoded)) return null;
  const serialized: JsonObject = {};
  for (const [key, item] of Object.entries(decoded.value)) {
    const json = projectJsonValue(item);
    if (json !== undefined) serialized[key] = json;
  }
  return serialized;
}

export function asString<T>(value: T): string | null {
  return Option.getOrNull(decodeString(value));
}

export function asFiniteNumber<T>(value: T): number | null {
  return Option.getOrNull(decodeFinite(value));
}

export function asBoolean<T>(value: T): boolean | null {
  return Option.getOrNull(decodeBoolean(value));
}

export function asBigInt<T>(value: T): bigint | null {
  return Option.getOrNull(decodeBigInt(value));
}

export function asNonEmptyTrimmedString<T>(value: T): string | null {
  const text = asString(value);
  if (text === null) return null;
  const trimmed = text.trim();
  return trimmed.length > 0 ? trimmed : null;
}

export function asReferenceId<T>(value: T): string | null {
  const text = asNonEmptyTrimmedString(value);
  if (text !== null) return text;
  const number = asFiniteNumber(value);
  if (number !== null) return String(number);
  const bigint = asBigInt(value);
  return bigint === null ? null : String(bigint);
}

export function parseJsonObject<T>(value: T): JsonObject | null {
  return asRecord(value);
}

export function projectJsonObject<T>(value: T): JsonObject | null {
  return asRecord(value);
}

export function projectJsonValue<T>(value: T): JsonValue | undefined {
  if (Array.isArray(value)) {
    return value.map((item) => projectJsonValue(item) ?? null);
  }
  const primitive = decodeJsonPrimitive(value);
  if (Option.isSome(primitive)) return primitive.value;
  return asRecord(value) ?? undefined;
}
