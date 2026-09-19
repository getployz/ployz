import { Schema, SchemaGetter } from "effect";

/** A blank domain port follows the container's PORT rather than saving today's default. */
export const domainPortSchema = Schema.String.check(
  Schema.makeFilter((value) => {
    if (value.trim() === "") return true;
    const port = Number(value);
    return Number.isInteger(port) && port >= 1 && port <= 65_535;
  }, { message: "Enter a port between 1 and 65535, or leave blank to use PORT." }),
).pipe(
  Schema.decodeTo(Schema.NullOr(Schema.Int), {
    decode: SchemaGetter.transform((value) => value.trim() === "" ? null : Number(value)),
    encode: SchemaGetter.transform((value) => value === null ? "" : String(value)),
  }),
);
