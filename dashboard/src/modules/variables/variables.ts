/** A variable's value as the panel shows it: plain text, or sealed (never read back). */
export type VariableValue =
  | { type: "plain"; value: string }
  | { type: "sealed"; /** A secret still without a value (it arrived by Sync): deploys skip it until it is set. */ notSet?: boolean };

/** One of a Service's variables, as the variables panel shows and edits it. */
export type VariableRecord = {
  unresolvedReferences?: readonly string[];
  /** The next Deploy changes it: the pink trail. */
  changed?: boolean;
  /** Its Environment marks it Never sync: Sync never carries it in or out. */
  neverSynced?: boolean;
  /** A rename of it is pending: it takes no edit until the Store answers, as one would name the old key. */
  renaming?: boolean;
  id: string;
  serviceId: string;
  key: string;
  description: string | null;
  exported: boolean;
  value: VariableValue;
};

export type PlainVariableRecord = Omit<VariableRecord, "value"> & { value: Extract<VariableValue, { type: "plain" }> };

/** Where the panel's writes go: each shows at once and persists in the background. */
export type VariableWriter = {
  insert(variable: VariableRecord): { isPersisted: { promise: Promise<unknown> } };
  update(variableId: string, updater: (draft: VariableRecord) => void): { isPersisted: { promise: Promise<unknown> } };
  delete(variableId: string): { isPersisted: { promise: Promise<unknown> } };
};

/** A variable's name, as the Store takes it (the settings catalog's `env` keys). */
const VARIABLE_KEY = /^[A-Z_][A-Z0-9_]{0,127}$/u;

/** Why `key`, typed and uppercased, can't name a variable; null when it can. */
export function variableKeyError(key: string) {
  return VARIABLE_KEY.test(key) ? null : "Use letters, digits and underscores, not starting with a digit (at most 128).";
}
