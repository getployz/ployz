/** A request the minter will not serve. The reason goes to the caller and the audit log, so it never holds a value. */
export class Refusal extends Error {
  constructor(readonly reason: string) {
    super(reason);
  }
}

export type Field<T> = { optional: boolean; read: (value: unknown, name: string) => T };
export type Shape = Record<string, Field<unknown>>;
export type Parsed<S extends Shape> = { [K in keyof S]: S[K] extends Field<infer T> ? T : never };

const required = <T>(read: Field<T>["read"]): Field<T> => ({ optional: false, read });

export function optional<T>(field: Field<T>): Field<T | undefined> {
  return { optional: true, read: field.read };
}

/** Parses an object with exactly these fields. An unknown field is refused, never ignored. */
export function strict<S extends Shape>(value: unknown, shape: S, name = "body"): Parsed<S> {
  if (!isPlainObject(value)) throw new Refusal(`${name} must be an object`);
  for (const key of Object.keys(value)) {
    if (!Object.hasOwn(shape, key)) throw new Refusal(`${name} has unknown field ${key.slice(0, 64)}`);
  }
  const parsed: Record<string, unknown> = {};
  for (const [key, field] of Object.entries(shape)) {
    const present = Object.hasOwn(value, key);
    if (!present && !field.optional) throw new Refusal(`${name} is missing ${key}`);
    if (present) parsed[key] = field.read(value[key], key);
  }
  return parsed as Parsed<S>;
}

export function isPlainObject(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

export const id = required((value, name) => {
  if (typeof value !== "number" || !Number.isSafeInteger(value) || value < 1) throw new Refusal(`${name} must be a positive integer`);
  return value;
});

export const ids = (max: number) => required((value, name) => {
  if (!Array.isArray(value) || value.length === 0 || value.length > max) throw new Refusal(`${name} must hold 1 to ${max} IDs`);
  return value.map((each) => id.read(each, name));
});

const HEX40 = /^[0-9a-f]{40}$/;

export const sha = required((value, name) => {
  if (typeof value !== "string" || !HEX40.test(value)) throw new Refusal(`${name} must be a 40-hex commit`);
  return value;
});

export const text = (max: number, pattern?: RegExp) => required((value, name) => {
  if (typeof value !== "string" || value.length === 0 || value.length > max || (pattern && !pattern.test(value))) {
    throw new Refusal(`${name} is not a valid value`);
  }
  return value;
});

export const oneOf = <const T extends string>(values: readonly T[]) => required((value, name) => {
  if (typeof value !== "string" || !(values as readonly string[]).includes(value)) throw new Refusal(`${name} is not one of ${values.join(", ")}`);
  return value as T;
});

export const object = required((value, name) => {
  if (!isPlainObject(value)) throw new Refusal(`${name} must be an object`);
  return value;
});

export const httpsUrl = required((value, name) => {
  if (typeof value !== "string" || value.length > 2048 || !URL.canParse(value) || new URL(value).protocol !== "https:") {
    throw new Refusal(`${name} must be an https URL`);
  }
  return value;
});

/**
 * A branch name as `git check-ref-format --branch` takes it, minus the forms the dispatch route refuses outright: a
 * full ref such as `refs/tags/v1`, and a 40-hex name GitHub could read as a commit.
 */
export const branch = required((value, name) => {
  if (typeof value !== "string" || value.length === 0 || value.length > 255) throw new Refusal(`${name} is not a branch`);
  if (value.startsWith("refs/")) throw new Refusal(`${name} must be a branch name, not a ref`);
  if (HEX40.test(value)) throw new Refusal(`${name} must be a branch name, not a commit`);
  const invalid = /[\x00-\x20\x7f~^:?*[\\]/.test(value) || value.includes("..") || value.includes("@{") || value.includes("//")
    || value.startsWith("/") || value.startsWith("-") || value.endsWith("/") || value.endsWith(".") || value.endsWith(".lock")
    || value === "@" || value.split("/").some((part) => part.startsWith("."));
  if (invalid) throw new Refusal(`${name} is not a branch`);
  return value;
});
