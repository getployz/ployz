interface PathWalkRecord {
  [key: string]:
    | PathWalkRecord
    | Date
    | string
    | number
    | boolean
    | bigint
    | symbol
    | null
    | undefined
    | readonly unknown[];
}

type AnyRecord = PathWalkRecord;
export type Primitive = string | number | boolean | bigint | symbol | null | undefined;

export type DeepPath<T> = T extends Primitive | Date
  ? never
  : {
      [K in Extract<keyof T, string>]: NonNullable<T[K]> extends
        | Primitive
        | Date
        ? K
        : NonNullable<T[K]> extends readonly unknown[]
          ? never
          : NonNullable<T[K]> extends object
            ? `${K}.${DeepPath<NonNullable<T[K]>>}`
            : K;
    }[Extract<keyof T, string>];

export type DeepPathValue<T, TPath extends string> = TPath extends
  `${infer THead}.${infer TTail}`
  ? THead extends keyof T
    ? DeepPathValue<NonNullable<T[THead]>, TTail>
    : never
  : TPath extends keyof T
    ? T[TPath]
    : never;

/**
 * Like `DeepPath` but also allows pointing to object and array values directly,
 * not just primitive leaves. Used for deployment diff field paths where you need
 * to track whole objects (e.g. discriminated union branches like `source.branch`).
 */
export type WidePath<T> = T extends Primitive | Date
  ? never
  : {
      [K in Extract<keyof T, string>]: NonNullable<T[K]> extends
        | Primitive
        | Date
        | readonly unknown[]
        ? K
        : NonNullable<T[K]> extends object
          ? K | `${K}.${WidePath<NonNullable<T[K]>>}`
          : K;
    }[Extract<keyof T, string>];

/**
 * Like `DeepPathValue` but distributes over union types, which is required when
 * the path traverses a discriminated union (e.g. `ServiceDeploymentConfig["source"]`).
 */
export type WidePathValue<T, TPath extends string> = T extends unknown
  ? TPath extends `${infer THead}.${infer TTail}`
    ? THead extends keyof T
      ? WidePathValue<NonNullable<T[THead]>, TTail>
      : never
    : TPath extends keyof T
      ? T[TPath]
      : never
  : never;

export type TextInputPath<T> = {
  [TPath in DeepPath<T>]: DeepPathValue<T, TPath> extends
    | string
    | null
    | undefined
    ? TPath
    : never;
}[DeepPath<T>];

function getSegments(path: string) {
  return path.split(".").filter(Boolean);
}

function asWalkRecord<T>(value: T): PathWalkRecord | null {
  if (
    value == null ||
    Array.isArray(value) ||
    value instanceof Date ||
    value instanceof Function
  ) {
    return null;
  }
  if (Object(value) !== value) {
    return null;
  }
  const record: PathWalkRecord = {};
  Object.assign(record, value);
  return record;
}

export function areDeepEqual<TLeft, TRight>(left: TLeft, right: TRight): boolean {
  if (Object.is(left, right)) {
    return true;
  }

  if (left == null || right == null) {
    return left == null && right == null;
  }

  if (left instanceof Date || right instanceof Date) {
    return left instanceof Date &&
      right instanceof Date &&
      left.getTime() === right.getTime();
  }

  if (Array.isArray(left) || Array.isArray(right)) {
    if (!Array.isArray(left) || !Array.isArray(right)) {
      return false;
    }

    return (
      left.length === right.length &&
      left.every((value, index) => areDeepEqual(value, right[index]))
    );
  }

  const leftRecord = asWalkRecord(left);
  const rightRecord = asWalkRecord(right);
  if (leftRecord !== null && rightRecord !== null) {
    const leftEntries = Object.entries(leftRecord);
    const rightEntries = Object.entries(rightRecord);

    if (leftEntries.length !== rightEntries.length) {
      return false;
    }

    const rightByKey = new Map(rightEntries);

    return leftEntries.every(([key, value]) => {
      if (!rightByKey.has(key)) {
        return false;
      }

      return areDeepEqual(value, rightByKey.get(key));
    });
  }

  return false;
}

export function getValueAtPath<TValue, TPath extends DeepPath<TValue>>(
  value: TValue,
  path: TPath,
): DeepPathValue<TValue, TPath> {
  // SAFETY: walking string segments loses DeepPathValue; path is already DeepPath<TValue>.
  return getSegments(path).reduce((current: PathWalkRecord[string] | TValue, segment) => {
    const record = asWalkRecord(current);
    if (record === null) {
      return undefined;
    }

    return record[segment];
  }, value) as DeepPathValue<TValue, TPath>;
}

export function setDraftValueAtPath<TValue, TPath extends DeepPath<TValue>>(
  value: TValue,
  path: TPath,
  nextValue: DeepPathValue<TValue, TPath>,
): void {
  const segments = getSegments(path);

  const root = asWalkRecord(value);
  if (segments.length === 0 || root === null) {
    return;
  }

  let current: AnyRecord = root;

  for (const segment of segments.slice(0, -1)) {
    const next = current[segment];

    if (asWalkRecord(next) === null) {
      current[segment] = {};
    }

    // SAFETY: after ensuring the child is a walk record (or assigning {}), the slot is an AnyRecord.
    current = current[segment] as AnyRecord;
  }

  const lastSegment = segments[segments.length - 1];
  if (lastSegment === undefined) {
    return;
  }
  // SAFETY: DeepPathValue is not in PathWalkRecord's value union; the path already selected this slot.
  current[lastSegment] = nextValue as PathWalkRecord[string];
}
