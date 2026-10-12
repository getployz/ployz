import { Schema, SchemaGetter } from "effect";

export const VOLUME_PAGES = [
  { id: "settings", label: "Settings" },
  { id: "backups", label: "Backups" },
] as const;

export type VolumePage = (typeof VOLUME_PAGES)[number]["id"];

export const volumePageSchema = Schema.Literals(VOLUME_PAGES.map((page) => page.id));

/** The open tab. A tab this drawer doesn't have, say one carried over from another drawer, opens Settings. */
export const volumeSearchSchema = Schema.Struct({
  tab: Schema.optional(Schema.String.pipe(Schema.decodeTo(volumePageSchema, {
    decode: SchemaGetter.transform((tab) => Schema.is(volumePageSchema)(tab) ? tab : "settings"),
    encode: SchemaGetter.transform((tab) => tab),
  }))),
});
