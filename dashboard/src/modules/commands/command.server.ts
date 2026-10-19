import "@tanstack/react-start/server-only";
import type { JsonValue } from "@ployz/sdk";
import { Effect, Schema, SchemaIssue } from "effect";
import type { JsonObject } from "#/db/tables";
import { asRecord, asString } from "#/lib/json";
import { gateOperation, type OperationAsked } from "#/modules/approvals/approvals.server";
import { type CommandRefusal, type StoreRefusal, StoreRefused } from "#/modules/config-store/store.contract";
import type { Caller } from "#/modules/identity/actor";
import { lacks } from "#/modules/organization/access.server";
import type { Permission } from "#/modules/organization/permissions";
import { NotFound } from "#/server/public-error";
import type { Database } from "#/server/database.server";

/** A command's answer: its output and an optional line for a human, or the refusal `ployz` prints. */
export type Ran<T> = { ok: true; output: T; message?: string } | { ok: false; refusal: StoreRefusal };

/** What a call to a command answers: its output, or its refusal in the shape Store tools refuse. */
export type Called<T> = { ok: true; output: T; message?: string } | { ok: false; refusal: CommandRefusal };

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
  | { readonly kind: "read"; readonly gate?: never; readonly removes?: never }
  /** `removes`: it removes or revokes something, so agent hosts mark it destructive. */
  | { readonly kind: "write"; readonly gate: Gate<F, E, R>; readonly removes: boolean };

type Shown = {
  readonly path: readonly string[];
  readonly about: string;
  readonly examples: readonly string[];
  readonly requires: Permission;
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
  readonly removes: boolean;
  readonly positional: readonly string[];
  readonly input: Schema.Struct<Fields>;
  readonly output: O;
  readonly invoke: (caller: Caller, raw: JsonValue, call: CommandCall) => Effect.Effect<Called<O["Type"]>, Exclude<E, Refusable>, R | Database>;
};

export const refused = (code: string, message: string, details: JsonValue = null) =>
  ({ ok: false, refusal: { code, message, details } }) as const;

const issues = SchemaIssue.makeFormatterStandardSchemaV1();

type Problem = { readonly message: string; readonly path?: ReadonlyArray<PropertyKey | { readonly key: PropertyKey }> | undefined };

/**
 * `invalid_argument` at the first problem's field, `expected` the schema that fits there. An input that isn't an object,
 * or names a field the command doesn't take, names no field it could fix: `expected` is the whole input's schema.
 */
const invalidInput = (schema: JsonObject, problems: ReadonlyArray<Problem>, fits: "field" | "input" = "field") => {
  const path = (problems[0]?.path ?? []).map((segment) => String(asRecord(segment)?.["key"] ?? segment));
  const atField = path.reduce<JsonValue>((at, key) => asRecord(asRecord(at)?.["properties"] ?? null)?.[key] ?? null, schema);
  const message = problems.map((problem) => problem.message).join("; ");
  const expected = fits === "field" && atField !== null ? atField : schema;
  return { ok: false, refusal: { code: "invalid_argument", message, details: null, field: path.join("."), expected } } as const;
};

const EMPTY_INPUT: JsonObject = { type: "object", properties: {}, required: [], additionalProperties: false };

/** `{anyOf: [{type: "string"}, {type: "null"}]}` as `{type: ["string", "null"]}`, the one form `ployz` reads. */
function nullableAsTypes(value: JsonValue): JsonValue {
  if (Array.isArray(value)) return value.map(nullableAsTypes);
  const node = asRecord(value);
  if (node === null) return value;
  const walked = Object.fromEntries(Object.entries(node).map(([key, item]) => [key, nullableAsTypes(item)]));
  const [first, second, ...rest] = Array.isArray(walked["anyOf"]) ? walked["anyOf"] : [];
  const nonNull = asRecord(first ?? null);
  const isNull = asRecord(second ?? null);
  const type = asString(nonNull?.["type"] ?? null);
  if (rest.length > 0 || nonNull === null || isNull === null || type === null || Object.keys(nonNull).length !== 1) return walked;
  if (Object.keys(isNull).length !== 1 || isNull["type"] !== "null") return walked;
  const { anyOf: _anyOf, ...siblings } = walked;
  return { ...siblings, type: [type, "null"] };
}

export function jsonSchema(schema: Schema.Top): JsonObject {
  return asRecord(nullableAsTypes(asRecord(Schema.toJsonSchemaDocument(schema).schema))) ?? {};
}

/** Effect writes an empty struct as "object or array" and drops an empty `required`; `ployz` reads neither. */
export function inputSchema(input: Schema.Struct<Fields>): JsonObject {
  if (Object.keys(input.fields).length === 0) return EMPTY_INPUT;
  const schema = jsonSchema(input);
  return { ...schema, required: schema["required"] ?? [] };
}

export function defineCommand<const N extends string, F extends Fields, O extends Schema.Top, E = never, R = never>(def: CommandDef<N, F, O, E, R>): Command<N, O, E, R> {
  const { run, input, kind } = def;
  const gate = def.kind === "write" ? def.gate : "never";
  const decode = Schema.decodeUnknownEffect(input);
  const invoke = (caller: Caller, raw: JsonValue, call: CommandCall): Effect.Effect<Called<O["Type"]>, Exclude<E, Refusable>, R | Database> => {
    let started = false;
    // SAFETY: the catchIfs below remove every NotFound and StoreRefused; TypeScript cannot subtract them from a generic `E`.
    return Effect.gen(function* () {
      const fields = asRecord(raw);
      if (fields === null) return invalidInput(inputSchema(input), [{ message: "Expected the input as an object" }], "input");
      const unknown = Object.keys(fields).filter((key) => !Object.hasOwn(input.fields, key));
      if (unknown.length > 0) return invalidInput(inputSchema(input), unknown.map((key) => ({ message: `Unknown input ${key}`, path: [key] })), "input");
      const decoded = yield* decode(fields).pipe(Effect.result);
      if (decoded._tag === "Failure") return invalidInput(inputSchema(input), issues(decoded.failure.issue).issues);
      // Every published write lands in the Organization (none names an Environment), so a token limited to Environments can't make one.
      const lacking = lacks(caller, def.requires, kind === "read" ? "reads" : "organization");
      if (lacking !== null) return refused("forbidden", lacking);
      if (gate !== "never") {
        const gated = yield* gateOperation(caller, call.approvalId, yield* gate.asked(caller, decoded.success));
        if (!gated.ok) return gated;
      }
      started = true;
      return yield* run(caller, decoded.success);
    }).pipe(
      Effect.catchIf((failure) => failure instanceof NotFound, (missing) => Effect.succeed(refused("not_found", missing.message))),
      Effect.catchIf((failure) => failure instanceof StoreRefused, (store) => Effect.succeed({ ok: false, refusal: store.refusal } as const)),
      Effect.map((ran): Called<O["Type"]> => ran.ok ? ran : { ok: false, refusal: { ...ran.refusal, writes: started && kind === "write" ? "unknown" : "none" } }),
      Effect.withSpan("Commands.invoke", { attributes: { command: def.name } }),
    ) as Effect.Effect<Called<O["Type"]>, Exclude<E, Refusable>, R | Database>;
  };
  const { run: _run, ...shown } = def;
  return { ...shown, kind, gate: gate === "never" ? "never" : "ask", removes: def.kind === "write" && def.removes, invoke };
}
