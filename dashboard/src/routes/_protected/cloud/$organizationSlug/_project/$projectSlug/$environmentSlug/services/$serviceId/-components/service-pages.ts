import { Effect, Option, Schema, SchemaGetter } from "effect";
import { RangeEdge } from "#/modules/uptime/uptime-range";

export const SERVICE_PAGES = [
  { id: "deployments", label: "Deployments" },
  { id: "variables", label: "Variables" },
  { id: "logs", label: "Logs" },
  { id: "monitoring", label: "Monitoring" },
  { id: "settings", label: "Settings" },
] as const;

export type ServicePage = (typeof SERVICE_PAGES)[number]["id"];

export const servicePageSchema = Schema.Literals(SERVICE_PAGES.map((page) => page.id));

/** The open tab. A tab this drawer doesn't have, say one carried over from another drawer, opens Settings. */
export const serviceSearchSchema = Schema.Struct({
  tab: Schema.optional(Schema.String.pipe(Schema.decodeTo(servicePageSchema, {
    decode: SchemaGetter.transform((tab) => Schema.is(servicePageSchema)(tab) ? tab : "settings"),
    encode: SchemaGetter.transform((tab) => tab),
  }))),
  from: Schema.optional(RangeEdge.pipe(Schema.catchDecoding(() => Effect.succeed(Option.none())))),
  to: Schema.optional(RangeEdge.pipe(Schema.catchDecoding(() => Effect.succeed(Option.none())))),
});
