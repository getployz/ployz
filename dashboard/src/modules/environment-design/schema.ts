import { Option, Schema, SchemaAST } from "effect";

export const strictParseOptions = {
  onExcessProperty: "error",
} satisfies SchemaAST.ParseOptions;

export function decodeStrict<
  S extends Schema.ConstraintDecoder<unknown>,
  Input,
>(
  schema: S,
  input: Input,
): S["Type"] {
  return Schema.decodeUnknownSync(schema)(input, strictParseOptions);
}

export function isValid<
  S extends Schema.ConstraintDecoder<unknown>,
  Input,
>(
  schema: S,
  input: Input,
): boolean {
  return Option.isSome(
    Schema.decodeUnknownOption(schema)(input, strictParseOptions),
  );
}

export type StringSchema = Schema.Codec<string, string>;

export type DeepMutable<T> = T extends Date
  ? T
  : T extends ReadonlyArray<infer Item>
    ? Array<DeepMutable<Item>>
    : T extends object
      ? { -readonly [Key in keyof T]: DeepMutable<T[Key]> }
      : T;

export function trimmedString(options?: {
  readonly minLength?: number;
  readonly maxLength?: number;
  readonly requiredMessage?: string;
  readonly maxLengthMessage?: string;
}) {
  const checks = [];
  if (options?.minLength !== undefined) {
    checks.push(
      Schema.isMinLength(options.minLength, {
        message: options.requiredMessage,
      }),
    );
  }
  if (options?.maxLength !== undefined) {
    checks.push(
      Schema.isMaxLength(options.maxLength, {
        message: options.maxLengthMessage,
      }),
    );
  }
  const [first, ...rest] = checks;
  return first === undefined ? Schema.Trim : Schema.Trim.check(first, ...rest);
}

export const Uuid = Schema.String.check(Schema.isUUID());

export function finiteNumber(options?: {
  readonly integer?: boolean;
  readonly minimum?: number;
  readonly maximum?: number;
}) {
  const checks = [];
  if (options?.integer === true) checks.push(Schema.isInt());
  if (options?.minimum !== undefined) {
    checks.push(Schema.isGreaterThanOrEqualTo(options.minimum));
  }
  if (options?.maximum !== undefined) {
    checks.push(Schema.isLessThanOrEqualTo(options.maximum));
  }
  const [first, ...rest] = checks;
  return first === undefined ? Schema.Finite : Schema.Finite.check(first, ...rest);
}
