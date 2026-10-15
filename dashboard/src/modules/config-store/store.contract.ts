import type { Change, ConfigCommand, ConfigQuery, ConfigView, ConfigWritten, EnvironmentRef as StoreEnvironmentRef, JsonValue } from "@ployz/sdk";
import { Data, Schema } from "effect";

/** A Store refusal as the Store words it: the RPC error vocabulary, never the rejected value. */
export type StoreRefusal = { code: string; message: string; details: JsonValue };

/**
 * The Store refused a call, in the RPC error vocabulary: `conflict` carries fresh state, `not_found` names what's
 * missing. Cloud fails with it; the browser's writer rejects with it.
 */
export class StoreRefused extends Data.TaggedError("StoreRefused")<StoreRefusal> {
  get refusal(): StoreRefusal {
    return { code: this.code, message: this.message, details: this.details };
  }
}

/**
 * What a Store read or write returns across the server-function boundary. A refusal is an outcome, not a failure:
 * `conflict` carries fresh state and `not_found` names what's missing, so callers need the code and details intact.
 */
export type StoreResult<T> = { ok: true; value: T } | { ok: false; refusal: StoreRefusal };

/** One view the tab shows, as the Store answers it after the write committed. */
export type ViewAnswer = { query: ConfigQuery; result: StoreResult<ConfigView> };

/** The most views one write asks the Store to answer; the tab refetches the rest. */
export const MAX_SHOWN_VIEWS = 16;

/**
 * The Environment a command names, the one its write queue is of; a Batch names one for all. A removal names its
 * Environment within the Project it removes from, and moves no views of one.
 */
export function commandEnvironment(command: ConfigCommand): StoreEnvironmentRef | null {
  if (command.command === "remove" || command.command === "cancel_removal") return null;
  return "environment" in command && command.environment ? command.environment : null;
}

/** Every Environment a command reads or writes: its own, a Sync's two sides, a Take's source, a Branch's Parent. */
export function commandEnvironments(command: ConfigCommand): StoreEnvironmentRef[] {
  switch (command.command) {
    case "sync": return command.into ? [command.from, command.into] : [command.from];
    case "take": return command.into ? [command.into, { project: command.into.project, environment: command.from }] : [{ project: null, environment: command.from }];
    case "create_branch": return [command.from];
    default: {
      const environment = commandEnvironment(command);
      return environment ? [environment] : [];
    }
  }
}

/** A dashboard write's answer: what it wrote and, once committed, the views the tab asked for. */
export type StoreWriteResult = { ok: true; value: ConfigWritten; answers?: ViewAnswer[] } | { ok: false; refusal: StoreRefusal };

/** One Store operation: a read answers a query with a view; a write applies a command. */
export type StoreCall = { operation: "read"; query: ConfigQuery } | { operation: "write"; command: ConfigCommand };

/** The view a query answers with: each query kind has the view of the same name. */
export type StoreViewOf<Q extends ConfigQuery> = Extract<ConfigView, { view: Q["query"] }>;

/** `view` if it is the one `query` answers with. */
export function viewOf<K extends ConfigQuery["query"]>(view: ConfigView, query: { query: K }): Extract<ConfigView, { view: K }> | undefined {
  const answers = (candidate: ConfigView): candidate is Extract<ConfigView, { view: K }> => candidate.view === query.query;
  return answers(view) ? view : undefined;
}

/** Reads one view in the calling Organization. */
export type StoreRead = <Q extends ConfigQuery>(query: Q) => Promise<StoreViewOf<Q>>;

/** What a call answers with: a read its query's view, a write what it wrote. */
export type StoreAnswer<C extends StoreCall> = C extends { operation: "read"; query: infer Q extends ConfigQuery } ? StoreViewOf<Q> : ConfigWritten;

/**
 * A query or command from outside Cloud, checked once where it enters: an object naming its kind. The Store decodes
 * and validates the rest itself, refusing anything else as `invalid_argument`.
 */
const envelope = <T>(kind: keyof T & string) => {
  const named = Schema.is(Schema.Struct({ [kind]: Schema.String }));
  return Schema.declare<T>((value): value is T => named(value));
};
export const StoreQuery = envelope<ConfigQuery>("query");
export const StoreCommand = envelope<ConfigCommand>("command");
export const StoreView = envelope<ConfigView>("view");
export const StoreChange = envelope<Change>("op");

/** An Environment as calls name it: either part left out means the default. */
export const EnvironmentRef = Schema.Struct({
  project: Schema.optional(Schema.NullOr(Schema.String)),
  environment: Schema.optional(Schema.NullOr(Schema.String)),
});

const isSyncShown = Schema.is(Schema.Struct({ from: EnvironmentRef, into: Schema.optional(Schema.NullOr(EnvironmentRef)) }));
const isEnvironmentShown = Schema.is(Schema.Struct({ environment: Schema.optional(Schema.NullOr(EnvironmentRef)) }));
const isQuery = Schema.is(StoreQuery);
/**
 * A view a write rereads after it commits: a query whose Environments are well formed, since Cloud picks the views to
 * reread by them before the Store sees the query. Checked before the write, so a bad one cannot fail a committed write.
 */
export const ShownQuery = Schema.declare<ConfigQuery>((value): value is ConfigQuery =>
  isQuery(value) && (value.query === "sync" ? isSyncShown(value) : isEnvironmentShown(value)));

export const storeReadInput = Schema.Struct({ organizationSlug: Schema.String, query: StoreQuery });
export const storeWriteInput = Schema.Struct({ organizationSlug: Schema.String, command: StoreCommand, shows: Schema.Array(ShownQuery) });
export const environmentOf = (ref: typeof EnvironmentRef.Type | undefined) =>
  ({ project: ref?.project ?? null, environment: ref?.environment ?? null });

export type { ConfigCommand, ConfigQuery, ConfigView, ConfigWritten };
