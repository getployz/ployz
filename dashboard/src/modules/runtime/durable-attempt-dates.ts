import { Schema } from "effect";

const DurableAttemptDates = Schema.Struct({
  createdAt: Schema.DateFromString,
  startedAt: Schema.NullOr(Schema.DateFromString),
  terminalAt: Schema.NullOr(Schema.DateFromString),
  updatedAt: Schema.DateFromString,
});

type SerializedDurableAttemptDates = {
  readonly createdAt: string;
  readonly startedAt: string | null;
  readonly terminalAt: string | null;
  readonly updatedAt: string;
};

export function reviveDurableAttemptDates<
  T extends SerializedDurableAttemptDates,
>(attempt: T) {
  const dates = Schema.decodeUnknownSync(DurableAttemptDates)(attempt);
  return { ...attempt, ...dates };
}
