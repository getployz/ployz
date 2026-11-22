import { Effect, Option, Schema } from "effect";
import { tabSearchSchema } from "#/lib/tab-search";
import { RangeEdge } from "#/lib/time-range";

export const SERVICE_PAGES = [
  { id: "deployments", label: "Deployments" },
  { id: "variables", label: "Variables" },
  { id: "logs", label: "Logs" },
  { id: "monitoring", label: "Monitoring" },
  { id: "settings", label: "Settings" },
] as const;

export type ServicePage = (typeof SERVICE_PAGES)[number]["id"];

export const servicePageSchema = Schema.Literals(SERVICE_PAGES.map((page) => page.id));

/** The open tab, which falls back to Settings, and the range the Monitoring tab shows. */
export const serviceSearchSchema = Schema.Struct({
  ...tabSearchSchema(SERVICE_PAGES, "settings").fields,
  from: Schema.optional(RangeEdge.pipe(Schema.catchDecoding(() => Effect.succeed(Option.none())))),
  to: Schema.optional(RangeEdge.pipe(Schema.catchDecoding(() => Effect.succeed(Option.none())))),
});
