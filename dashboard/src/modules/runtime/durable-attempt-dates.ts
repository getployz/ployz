// A step's result comes back as JSON (strings) on replay, but in-process with its Dates intact when Inngest
// checkpoints, so each date is taken either way.
type AttemptDate = Date | string;

type SerializedDurableAttemptDates = {
  readonly createdAt: AttemptDate;
  readonly startedAt: AttemptDate | null;
  readonly terminalAt: AttemptDate | null;
  readonly updatedAt: AttemptDate;
};

const toDate = (value: AttemptDate) => new Date(value);

export function reviveDurableAttemptDates<
  T extends SerializedDurableAttemptDates,
>(attempt: T) {
  return {
    ...attempt,
    createdAt: toDate(attempt.createdAt),
    startedAt: attempt.startedAt === null ? null : toDate(attempt.startedAt),
    terminalAt: attempt.terminalAt === null ? null : toDate(attempt.terminalAt),
    updatedAt: toDate(attempt.updatedAt),
  };
}
