import type { MachineId } from "@ployz/sdk";
import { Schema, SchemaAST } from "effect";

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

export type StringSchema = Schema.Codec<string, string>;

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

export const MACHINE_ID_PATTERN = /^[0-9a-f]{32}$/u;

/** The same rule as `MACHINE_ID_PATTERN`, for table checks. */
export const MACHINE_ID_SQL = "^[0-9a-f]{32}$";

const MachineIdType = Schema.declare<MachineId>(
  (value): value is MachineId => typeof value === "string",
);

/** A Machine ID: the daemon's 32-hex form, branded as the SDK's `MachineId`. */
export const rustMachineIdSchema = Schema.String.check(
  Schema.isPattern(MACHINE_ID_PATTERN, {
    message: "MachineId must be a 32-hex UUID",
  }),
).pipe(Schema.decodeTo(MachineIdType));
