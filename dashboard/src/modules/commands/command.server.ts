import "@tanstack/react-start/server-only";
import type { JsonValue } from "@ployz/sdk";
import { Effect, Schema, SchemaIssue } from "effect";
import { asRecord } from "#/lib/json";
import { gateOperation, type OperationAsked } from "#/modules/approvals/approvals.server";
import { type StoreRefusal, StoreRefused } from "#/modules/config-store/store.contract";
import type { Caller } from "#/modules/identity/actor";
import { NotFound } from "#/server/public-error";
import type { Database } from "#/server/database.server";

/** A command's answer: its output and an optional line for a human, or the refusal `ployz` prints. */
export type Ran<T> = { ok: true; output: T; message?: string } | { ok: false; refusal: StoreRefusal };

/**
 * How one call arrived. `key` is the `x-ployz-request-key` the CLI sends; no attempt is recorded under it, because every
 * published write converges on its own when retried.
 */
export type CommandCall = { readonly approvalId: string | null; readonly key: string | null };

/** Input fields decode with no services: a published command parses its input before it touches anything. */
type Fields = { readonly [x: PropertyKey]: Schema.Struct.Fields[PropertyKey] & { readonly DecodingServices: never } };
type InputOf<F extends Fields> = Schema.Struct<F>["Type"];
/** Failures every command answers as a refusal instead of an error. */
type Refusable = NotFound | StoreRefused;
type Effectful<A, E, R> = Effect.Effect<A, E | Refusable, R>;

type Gate<F extends Fields, E, R> =
  | "never"
  | { readonly gate: "ask"; readonly asked: (caller: Caller, input: InputOf<F>) => Effectful<OperationAsked, E, R> };

type Kind<F extends Fields, E, R> =
  | { readonly kind: "read"; readonly gate?: never }
  | { readonly kind: "write"; readonly gate: Gate<F, E, R> };

type Shown = {
  readonly path: readonly string[];
  readonly about: string;
  readonly examples: readonly string[];
  readonly agents: boolean;
  readonly typed: "organization" | null;
  readonly render: "table" | "record" | "message";
};

export type CommandDef<N extends string, F extends Fields, O extends Schema.Top, E, R> = Kind<F, E, R> & Shown & {
  readonly name: N;
  readonly positional: ReadonlyArray<keyof F & string>;
  readonly input: Schema.Struct<F>;
  readonly output: O;
  readonly run: (caller: Caller, input: InputOf<F>) => Effectful<Ran<O["Type"]>, E, R>;
};

/** A registered command: what the manifest shows of it, and how a call runs. */
export type Command<N extends string = string, O extends Schema.Top = Schema.Top, E = never, R = never> = Shown & {
  readonly name: N;
  readonly kind: "read" | "write";
  readonly gate: "never" | "ask";
  readonly positional: readonly string[];
  readonly input: Schema.Struct<Fields>;
  readonly output: O;
  readonly invoke: (caller: Caller, raw: JsonValue, call: CommandCall) => Effect.Effect<Ran<O["Type"]>, Exclude<E, Refusable>, R | Database>;
};

export const refused = (code: string, message: string, details: JsonValue = null) =>
  ({ ok: false, refusal: { code, message, details } }) as const;

const issues = SchemaIssue.makeFormatterStandardSchemaV1();

const invalidInput = (problems: ReadonlyArray<{ readonly message: string; readonly path?: ReadonlyArray<PropertyKey | { readonly key: PropertyKey }> | undefined }>) =>
  refused("invalid_input", problems.map((problem) => problem.message).join("; "), {
    issues: problems.map(({ message, path }) => ({
      message,
      path: (path ?? []).map((segment) => String(asRecord(segment)?.["key"] ?? segment)),
    })),
  });

export function defineCommand<const N extends string, F extends Fields, O extends Schema.Top, E = never, R = never>(def: CommandDef<N, F, O, E, R>): Command<N, O, E, R> {
  const { run, input, kind } = def;
  const gate = def.kind === "write" ? def.gate : "never";
  const decode = Schema.decodeUnknownEffect(input);
  // SAFETY: the catchIfs below remove every NotFound and StoreRefused; TypeScript cannot subtract them from a generic `E`.
  const invoke = (caller: Caller, raw: JsonValue, call: CommandCall): Effect.Effect<Ran<O["Type"]>, Exclude<E, Refusable>, R | Database> => Effect.gen(function* () {
    const fields = asRecord(raw);
    if (fields === null) return invalidInput([{ message: "Expected the input as an object" }]);
    const unknown = Object.keys(fields).filter((key) => !Object.hasOwn(input.fields, key));
    if (unknown.length > 0) return invalidInput(unknown.map((key) => ({ message: `Unknown input ${key}`, path: [key] })));
    const decoded = yield* decode(fields).pipe(Effect.result);
    if (decoded._tag === "Failure") return invalidInput(issues(decoded.failure.issue).issues);
    if (gate !== "never") {
      const gated = yield* gateOperation(caller, call.approvalId, yield* gate.asked(caller, decoded.success));
      if (!gated.ok) return gated;
    }
    return yield* run(caller, decoded.success);
  }).pipe(
    Effect.catchIf((failure) => failure instanceof NotFound, (missing) => Effect.succeed(refused("not_found", missing.message))),
    Effect.catchIf((failure) => failure instanceof StoreRefused, (store) => Effect.succeed({ ok: false, refusal: store.refusal } as const)),
    Effect.withSpan("Commands.invoke", { attributes: { command: def.name } }),
  ) as Effect.Effect<Ran<O["Type"]>, Exclude<E, Refusable>, R | Database>;
  const { run: _run, ...shown } = def;
  return { ...shown, kind, gate: gate === "never" ? "never" : "ask", invoke };
}
