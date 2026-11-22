import { Effect } from "effect";

export function slugifySegment(value: string) {
  return value
    .trim()
    .toLowerCase()
    .normalize("NFKD")
    .replace(/[\u0300-\u036f]/g, "")
    .replace(/[^a-z0-9]+/g, "-")
    .replace(/^-+|-+$/g, "");
}

export function getSlugWithSuffix(base: string, attempt: number) {
  if (attempt === 0) {
    return base;
  }

  return `${base}-${crypto.randomUUID().slice(0, 8)}`;
}

const UNIQUE_ALLOCATION_ATTEMPTS = 5;

export function allocateUnique<A, E, R, Exhausted>(input: {
  readonly tryAttempt: (attempt: number) => Effect.Effect<A | null, E, R>;
  readonly exhausted: Exhausted;
}): Effect.Effect<A, E | Exhausted, R> {
  return Effect.gen(function* () {
    for (let attempt = 0; attempt < UNIQUE_ALLOCATION_ATTEMPTS; attempt += 1) {
      const created = yield* input.tryAttempt(attempt);
      if (created !== null) return created;
    }
    return yield* Effect.fail(input.exhausted);
  });
}
