import { useEffect, useState, type Dispatch, type SetStateAction } from "react";
import { Option, Schema } from "effect";

/** `localStorage` throws in a private window or when storage is full or blocked; the browser then just doesn't remember. */
export function readStored(key: string): string | null {
  try {
    return localStorage.getItem(key);
  } catch {
    return null;
  }
}

export function writeStored(key: string, value: string) {
  try {
    localStorage.setItem(key, value);
  } catch {
    // Not remembered; the page works without it.
  }
}

/**
 * State a browser remembers under `key`: read once when the component mounts, so a caller whose key changes remounts
 * (by `key`), and written whenever it changes. A value that is missing or fails `schema` starts as `empty`.
 */
export function useStoredState<A>(key: string, schema: Schema.Codec<A, string>, empty: A): [A, Dispatch<SetStateAction<A>>] {
  const [value, setValue] = useState<A>(() => Option.getOrElse(Schema.decodeUnknownOption(schema)(readStored(key)), () => empty));
  useEffect(() => {
    const text = Schema.encodeOption(schema)(value);
    if (Option.isSome(text)) writeStored(key, text.value);
  }, [key, schema, value]);
  return [value, setValue];
}
