import { Schema, SchemaAST } from "effect";
import { NonRetriableError } from "inngest";

export function decodeInngestEnvelope<S extends Schema.ConstraintDecoder<unknown>>(
  schema: S,
) {
  return <Input>(input: Input, options?: SchemaAST.ParseOptions): S["Type"] => {
    try {
      return Schema.decodeUnknownSync(schema)(input, options);
    } catch (cause) {
      throw new NonRetriableError("Inngest event envelope is invalid.", {
        cause: cause instanceof Error ? cause : undefined,
      });
    }
  };
}
