import { Schema, SchemaGetter } from "effect";

/** The open tab of a drawer. A tab the drawer doesn't have, say one carried over from another drawer, opens `fallback`. */
export function tabSearchSchema<const Id extends string>(pages: readonly { id: Id }[], fallback: NoInfer<Id>) {
  const pageSchema = Schema.Literals(pages.map((page) => page.id));
  return Schema.Struct({
    tab: Schema.optional(Schema.String.pipe(Schema.decodeTo(pageSchema, {
      decode: SchemaGetter.transform((tab) => Schema.is(pageSchema)(tab) ? tab : fallback),
      encode: SchemaGetter.transform((tab) => tab),
    }))),
  });
}
